//! The real Windows side of Optimize > Debloat. Reading is plain listing
//! (startup entries, scheduled tasks, services, this user's app packages).
//! The four change calls are the only writes, and `mod.rs` reaches them only
//! for items the reviewed list marks Safe to turn off and the guard allows.

use std::collections::{HashMap, HashSet};

use windows::core::{BSTR, HSTRING, PWSTR, VARIANT};
use windows::ApplicationModel::PackageSignatureKind;
use windows::Management::Deployment::{PackageManager, PackageTypes};
use windows::Win32::Foundation::{HANDLE, HLOCAL, LocalFree, VARIANT_BOOL};
use windows::Win32::Security::Authorization::ConvertSidToStringSidW;
use windows::Win32::Security::{GetTokenInformation, TokenUser, TOKEN_QUERY, TOKEN_USER};
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED};
use windows::Win32::System::TaskScheduler::{ITaskService, TaskScheduler};
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

use super::{Backend, Inventory, RawApp, RawService, RawStartup, RawTask};

pub struct Real;

struct Com(bool);

impl Drop for Com {
    fn drop(&mut self) {
        if self.0 {
            unsafe { CoUninitialize() };
        }
    }
}

fn com() -> Com {
    // S_OK and S_FALSE both need a CoUninitialize; RPC_E_CHANGED_MODE can still call.
    Com(unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok())
}

fn windows_dir() -> String {
    let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
    format!("{}\\", root.trim_end_matches('\\').to_lowercase())
}

/// The current user's SID as text ("S-1-5-21-...").
fn current_sid() -> Option<String> {
    unsafe {
        let mut token = HANDLE::default();
        OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).ok()?;
        let token = crate::actions::OwnedHandle(token);
        let mut needed = 0u32;
        let _ = GetTokenInformation(token.0, TokenUser, None, 0, &mut needed);
        if needed == 0 {
            return None;
        }
        let mut buf = vec![0u64; (needed as usize + 7) / 8];
        GetTokenInformation(token.0, TokenUser, Some(buf.as_mut_ptr() as *mut _), (buf.len() * 8) as u32, &mut needed).ok()?;
        let user = &*(buf.as_ptr() as *const TOKEN_USER);
        let mut text = PWSTR::null();
        ConvertSidToStringSidW(user.User.Sid, &mut text).ok()?;
        let out = text.to_string().ok();
        let _ = LocalFree(HLOCAL(text.0 as *mut _));
        out
    }
}

/// A task that runs as this user (Orbit can change it without admin).
fn runs_as_user(user_id: Option<&str>, sid: Option<&str>) -> bool {
    let Some(u) = user_id.map(str::trim).filter(|u| !u.is_empty()) else {
        return false;
    };
    let name = std::env::var("USERNAME").unwrap_or_default();
    if !name.is_empty() && (u.eq_ignore_ascii_case(&name) || u.to_lowercase().ends_with(&format!("\\{}", name.to_lowercase()))) {
        return true;
    }
    sid.is_some_and(|s| u.eq_ignore_ascii_case(s))
}

fn read_startup() -> Vec<RawStartup> {
    let mut entries = crate::startup::collect_startup();
    let file = crate::impact::load();
    crate::impact::apply(&mut entries, file.as_ref());
    let paths: Vec<String> = entries
        .iter()
        .filter(|e| e.enabled && e.impact.as_deref().is_some_and(|i| i != "low"))
        .filter_map(|e| e.path.clone())
        .collect();
    let sigs = if paths.is_empty() { HashMap::new() } else { crate::security::signatures_for(&paths) };
    let lookup = |p: &str| sigs.get(p).map(|s| (Some(s.status.clone()), s.publisher.clone())).unwrap_or((None, None));
    let suggested: HashSet<String> = crate::health::pick_suggestions(&entries, file.as_ref(), &lookup).into_iter().map(|s| s.id).collect();
    entries
        .into_iter()
        .map(|e| RawStartup {
            suggested: suggested.contains(&e.id),
            id: e.id,
            name: e.name,
            command: e.command,
            path: e.path,
            enabled: e.enabled,
            needs_admin: e.needs_admin,
            impact: e.impact,
        })
        .collect()
}

fn read_tasks() -> Result<Vec<RawTask>, String> {
    let sid = current_sid();
    Ok(crate::autoruns::all_tasks()?
        .into_iter()
        .map(|t| RawTask {
            needs_admin: t.path.to_lowercase().starts_with("\\microsoft\\") || !runs_as_user(t.user_id.as_deref(), sid.as_deref()),
            path: t.path,
            enabled: t.enabled,
            exe: t.exe,
            author: t.author,
        })
        .collect())
}

fn read_services() -> Result<Vec<RawService>, String> {
    let list = crate::services::collect_services()?;
    let auto: HashMap<String, String> = crate::autoruns::auto_services()
        .unwrap_or_default()
        .into_iter()
        .map(|s| (s.name.to_lowercase(), s.path))
        .collect();
    let win = windows_dir();
    Ok(list
        .into_iter()
        .map(|s| {
            let path = auto.get(&s.name.to_lowercase()).cloned().unwrap_or_default();
            let lower = path.to_lowercase();
            let starts = s.start_type == "auto" || s.start_type == "autoDelayed";
            RawService {
                third_party: starts && !path.is_empty() && !lower.starts_with(&win) && !lower.ends_with(".sys"),
                name: s.name,
                display: s.display_name,
                start_type: s.start_type,
                path,
            }
        })
        .collect())
}

fn read_apps() -> Result<Vec<RawApp>, String> {
    let _com = com();
    let pm = PackageManager::new().map_err(|e| format!("Could not open the app list ({e})."))?;
    // "" = the current user; Main = no frameworks, resources or bundles.
    let packages = pm
        .FindPackagesByUserSecurityIdWithPackageTypes(&HSTRING::new(), PackageTypes::Main)
        .map_err(|e| format!("Could not list your apps ({e})."))?;
    let mut out = Vec::new();
    for p in packages {
        let Ok(id) = p.Id() else { continue };
        let (Ok(name), Ok(full), Ok(family)) = (id.Name(), id.FullName(), id.FamilyName()) else { continue };
        let framework = p.IsFramework().unwrap_or(true) || p.IsResourcePackage().unwrap_or(false);
        let system = p.SignatureKind().map(|k| k == PackageSignatureKind::System).unwrap_or(true);
        let display = p.DisplayName().ok().map(|d| d.to_string()).filter(|d| !d.trim().is_empty() && !d.starts_with("ms-resource:"));
        out.push(RawApp { name: name.to_string(), full_name: full.to_string(), family: family.to_string(), display, system: framework || system });
    }
    Ok(out)
}

fn task_service() -> Result<ITaskService, String> {
    let service: ITaskService = unsafe { CoCreateInstance(&TaskScheduler, None, CLSCTX_INPROC_SERVER) }
        .map_err(|e| format!("Could not open Task Scheduler ({e})."))?;
    let empty = VARIANT::default();
    unsafe { service.Connect(&empty, &empty, &empty, &empty) }.map_err(|e| format!("Could not connect to Task Scheduler ({e})."))?;
    Ok(service)
}

fn task_error(e: &windows::core::Error) -> String {
    match e.code().0 as u32 {
        0x8007_0005 => "Access denied. Try Restart as administrator.".into(),
        0x8007_0002 | 0x8007_0003 => "This task no longer exists. Analyze again.".into(),
        _ => format!("Could not change the task ({e})."),
    }
}

impl Backend for Real {
    fn inventory(&self) -> Inventory {
        let mut inv = Inventory { startup: read_startup(), ..Default::default() };
        match read_tasks() {
            Ok(t) => inv.tasks = t,
            Err(e) => inv.errors.push(format!("Scheduled tasks: {e}")),
        }
        match read_services() {
            Ok(s) => inv.services = s,
            Err(e) => inv.errors.push(format!("Services: {e}")),
        }
        match read_apps() {
            Ok(a) => inv.apps = a,
            Err(e) => inv.errors.push(format!("Built-in apps: {e}")),
        }
        inv
    }

    fn is_admin(&self) -> bool {
        // SAFETY: no arguments; inspects the current token only.
        unsafe { windows::Win32::UI::Shell::IsUserAnAdmin() }.as_bool()
    }

    fn set_startup(&self, id: &str, enabled: bool) -> Result<(), String> {
        crate::startup::set_startup_enabled(id.to_string(), enabled)
    }

    fn set_task(&self, path: &str, enabled: bool) -> Result<(), String> {
        let _com = com();
        let service = task_service()?;
        let root = unsafe { service.GetFolder(&BSTR::from("\\")) }.map_err(|e| task_error(&e))?;
        let task = unsafe { root.GetTask(&BSTR::from(path)) }.map_err(|e| task_error(&e))?;
        unsafe { task.SetEnabled(VARIANT_BOOL::from(enabled)) }.map_err(|e| task_error(&e))
    }

    fn set_service_start(&self, name: &str, start: &str) -> Result<(), String> {
        crate::services::set_start_type(name, start)
    }

    fn remove_app(&self, full_name: &str) -> Result<(), String> {
        let _com = com();
        let pm = PackageManager::new().map_err(|e| format!("Could not open the app list ({e})."))?;
        // Without RemovalOptions this removes the package for the current user only.
        let result = pm
            .RemovePackageAsync(&HSTRING::from(full_name))
            .and_then(|op| op.get())
            .map_err(|e| format!("Windows could not remove the app ({e})."))?;
        let code = result.ExtendedErrorCode().map(|h| h.0).unwrap_or(0);
        if code != 0 {
            let text = result.ErrorText().map(|t| t.to_string()).unwrap_or_default();
            return Err(if text.trim().is_empty() { format!("Windows could not remove the app (0x{:08X}).", code as u32) } else { text.trim().to_string() });
        }
        Ok(())
    }

    fn app_installed(&self, family: &str) -> bool {
        let _com = com();
        let Ok(pm) = PackageManager::new() else { return false };
        pm.FindPackagesByUserSecurityIdPackageFamilyName(&HSTRING::new(), &HSTRING::from(family))
            .map(|it| it.into_iter().next().is_some())
            .unwrap_or(false)
    }

    fn open_store(&self, family: &str) -> Result<(), String> {
        if !super::valid_family(family) {
            return Err("Orbit doesn't know this app's Store page.".into());
        }
        crate::actions::shell_open(&super::store_url(family)).map_err(|_| "Could not open the Microsoft Store.".into())
    }
}
