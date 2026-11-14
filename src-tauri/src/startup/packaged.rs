//! Store (packaged, MSIX) startup tasks: apps such as WhatsApp or Claude that
//! start at sign-in through a `windows.startupTask` extension instead of a Run
//! key. They are read only in Orbit: Windows lets only the app itself, or the
//! user in Settings > Apps > Startup, change one.
//!
//! The task list comes from each installed package's `AppxManifest.xml`
//! (`PackageManager`, this user's packages). The on/off state comes from the
//! registry value Windows keeps per task:
//! `HKCU\Software\Classes\Local Settings\Software\Microsoft\Windows\CurrentVersion\
//! AppModel\SystemAppData\<PackageFamilyName>\<TaskId>`, value `State`, which
//! holds the `StartupTaskState` numbers (0 disabled, 1 disabled by user,
//! 2 enabled, 3 disabled by policy, 4 enabled by policy). A task Windows has
//! never touched has no key; then the manifest's `Enabled` attribute decides.

use std::collections::HashMap;
use std::sync::Mutex;

use windows::core::HSTRING;
use windows::Management::Deployment::{PackageManager, PackageTypes};
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED};

use super::*;

/// Id prefix of a packaged task: `Packaged\<PackageFamilyName>\<TaskId>`.
pub(crate) const PREFIX: &str = "Packaged\\";
/// What the Location column says.
pub(crate) const LOCATION: &str = "Store app";

const STATE_ROOT: &str = r"Software\Classes\Local Settings\Software\Microsoft\Windows\CurrentVersion\AppModel\SystemAppData";

// ---------------------------------------------------------------------------
// Manifest parsing (pure)
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub(super) struct ManifestTask {
    pub task_id: String,
    /// The manifest's `Enabled` attribute (the default before the user decides).
    pub enabled: bool,
    pub display_name: Option<String>,
    /// The extension's `Executable`, relative to the package.
    pub executable: Option<String>,
}

fn unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// The value of attribute `name` in a tag's text, or None.
fn attr(tag: &str, name: &str) -> Option<String> {
    let bytes = tag.as_bytes();
    let mut from = 0;
    while let Some(i) = tag[from..].find(name) {
        let start = from + i;
        let end = start + name.len();
        from = end;
        let before_ok = start > 0 && bytes[start - 1].is_ascii_whitespace();
        if !before_ok {
            continue;
        }
        let rest = tag[end..].trim_start();
        let Some(rest) = rest.strip_prefix('=') else { continue };
        let rest = rest.trim_start();
        let quote = rest.chars().next()?;
        if quote != '"' && quote != '\'' {
            continue;
        }
        let body = &rest[1..];
        let close = body.find(quote)?;
        return Some(unescape(&body[..close]));
    }
    None
}

/// The startup tasks an `AppxManifest.xml` declares. Comments are ignored.
pub(super) fn parse_manifest(xml: &str) -> Vec<ManifestTask> {
    let mut text = String::with_capacity(xml.len());
    let mut rest = xml;
    while let Some(i) = rest.find("<!--") {
        text.push_str(&rest[..i]);
        match rest[i..].find("-->") {
            Some(j) => rest = &rest[i + j + 3..],
            None => {
                rest = "";
                break;
            }
        }
    }
    text.push_str(rest);

    let mut out = Vec::new();
    let mut executable: Option<String> = None;
    let mut rest = text.as_str();
    while let Some(i) = rest.find('<') {
        let Some(j) = rest[i..].find('>') else { break };
        let tag = &rest[i + 1..i + j];
        rest = &rest[i + j + 1..];
        let name_end = tag.find(|c: char| c.is_whitespace() || c == '/').unwrap_or(tag.len());
        let local = tag[..name_end].rsplit(':').next().unwrap_or("");
        if local == "Extension" {
            executable = match attr(tag, "Category").as_deref() {
                Some("windows.startupTask") => attr(tag, "Executable"),
                _ => None,
            };
        } else if local == "StartupTask" {
            let Some(task_id) = attr(tag, "TaskId").filter(|t| !t.trim().is_empty()) else { continue };
            out.push(ManifestTask {
                task_id,
                enabled: attr(tag, "Enabled").is_some_and(|v| v.eq_ignore_ascii_case("true")),
                display_name: attr(tag, "DisplayName").filter(|d| !d.trim().is_empty()),
                executable: executable.clone(),
            });
        }
    }
    out
}

// ---------------------------------------------------------------------------
// State (pure)
// ---------------------------------------------------------------------------

/// The state name Orbit shows, from the registry's `State` number (if the task
/// has one) and the manifest default.
pub(super) fn state_name(registry: Option<u32>, manifest_enabled: bool) -> &'static str {
    match registry {
        Some(0) => "disabled",
        Some(1) => "disabledByUser",
        Some(2) => "enabled",
        Some(3) => "disabledByPolicy",
        Some(4) => "enabledByPolicy",
        _ if manifest_enabled => "enabled",
        _ => "disabled",
    }
}

pub(super) fn is_enabled(state: &str) -> bool {
    matches!(state, "enabled" | "enabledByPolicy")
}

fn read_state(family: &str, task_id: &str) -> Option<u32> {
    let key = RegKey::open(
        HKEY_CURRENT_USER,
        &format!(r"{STATE_ROOT}\{family}\{task_id}"),
        KEY_QUERY_VALUE | KEY_WOW64_64KEY,
    )
    .ok()?;
    let (ty, data) = key.query("State").ok()?;
    if ty != REG_DWORD || data.len() < 4 {
        return None;
    }
    Some(u32::from_le_bytes([data[0], data[1], data[2], data[3]]))
}

// ---------------------------------------------------------------------------
// Entries
// ---------------------------------------------------------------------------

/// One packaged task with the package it belongs to.
#[derive(Clone, Debug)]
pub(super) struct PkgTask {
    pub family: String,
    /// The package's own name when the task has no readable display name.
    pub package_name: String,
    pub task: ManifestTask,
}

/// The entry for a task in a given state. Names that would repeat inside one
/// package get the task id appended.
pub(super) fn build_entries(tasks: &[PkgTask], state: &dyn Fn(&str, &str) -> Option<u32>) -> Vec<StartupEntry> {
    let mut entries: Vec<StartupEntry> = tasks
        .iter()
        .map(|t| {
            let name = t
                .task
                .display_name
                .clone()
                .filter(|d| !d.starts_with("ms-resource:"))
                .unwrap_or_else(|| t.package_name.clone());
            let st = state_name(state(&t.family, &t.task.task_id), t.task.enabled);
            StartupEntry {
                id: format!("{PREFIX}{}\\{}", t.family, t.task.task_id),
                name,
                command: t.task.executable.clone().unwrap_or_default(),
                location: LOCATION.to_string(),
                enabled: is_enabled(st),
                path: None,
                needs_admin: false,
                impact: None,
                impact_measured_at: None,
                packaged_state: Some(st.to_string()),
            }
        })
        .collect();
    let mut seen: HashMap<(String, String), usize> = HashMap::new();
    for (e, t) in entries.iter().zip(tasks) {
        *seen.entry((t.family.clone(), e.name.to_lowercase())).or_default() += 1;
    }
    for (e, t) in entries.iter_mut().zip(tasks) {
        if seen[&(t.family.clone(), e.name.to_lowercase())] > 1 {
            e.name = format!("{} ({})", e.name, t.task.task_id);
        }
    }
    entries.sort_by_cached_key(|e| e.name.to_lowercase());
    entries
}

struct Com(bool);

impl Drop for Com {
    fn drop(&mut self) {
        if self.0 {
            unsafe { CoUninitialize() };
        }
    }
}

/// Task definitions found the last time the packages were listed, so the
/// file watcher can re-read states without listing every package again.
static TASKS: Mutex<Option<Vec<PkgTask>>> = Mutex::new(None);
/// Parsed manifests by package full name (a manifest never changes for one full name).
static MANIFESTS: Mutex<Option<HashMap<String, Vec<ManifestTask>>>> = Mutex::new(None);

fn list_tasks() -> Result<Vec<PkgTask>, String> {
    // S_OK and S_FALSE both need a CoUninitialize.
    let _com = Com(unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok());
    let pm = PackageManager::new().map_err(|e| format!("Could not open the app list ({e})."))?;
    // "" = the current user; Main = no frameworks, resources or bundles.
    let packages = pm
        .FindPackagesByUserSecurityIdWithPackageTypes(&HSTRING::new(), PackageTypes::Main)
        .map_err(|e| format!("Could not list your apps ({e})."))?;
    let mut cache = MANIFESTS.lock().unwrap_or_else(|e| e.into_inner());
    let cache = cache.get_or_insert_with(HashMap::new);
    let mut out = Vec::new();
    let mut seen_full = Vec::new();
    for p in packages {
        let Ok(id) = p.Id() else { continue };
        let (Ok(full), Ok(family), Ok(name)) = (id.FullName(), id.FamilyName(), id.Name()) else { continue };
        let full = full.to_string();
        seen_full.push(full.clone());
        let tasks = match cache.get(&full) {
            Some(t) => t.clone(),
            None => {
                let Some(dir) = p.InstalledPath().ok().map(|d| d.to_string()).filter(|d| !d.is_empty()) else { continue };
                // Unreadable or missing manifest: no tasks, and not cached (try again next time).
                let Ok(xml) = std::fs::read_to_string(Path::new(&dir).join("AppxManifest.xml")) else { continue };
                let parsed = parse_manifest(&xml);
                cache.insert(full.clone(), parsed.clone());
                parsed
            }
        };
        if tasks.is_empty() {
            continue;
        }
        let package_name = p
            .DisplayName()
            .ok()
            .map(|d| d.to_string())
            .filter(|d| !d.trim().is_empty() && !d.starts_with("ms-resource:"))
            .unwrap_or_else(|| name.to_string());
        for task in tasks {
            out.push(PkgTask { family: family.to_string(), package_name: package_name.clone(), task });
        }
    }
    // Forget manifests of packages that are gone.
    cache.retain(|k, _| seen_full.contains(k));
    Ok(out)
}

/// This user's packaged startup tasks with their current state. `refresh`
/// lists the packages again; without it the last list is reused (states are
/// always read fresh).
pub(crate) fn collect_packaged_startup(refresh: bool) -> Vec<StartupEntry> {
    let cached = if refresh { None } else { TASKS.lock().unwrap_or_else(|e| e.into_inner()).clone() };
    let tasks = match cached {
        Some(t) => t,
        None => match list_tasks() {
            Ok(t) => {
                *TASKS.lock().unwrap_or_else(|e| e.into_inner()) = Some(t.clone());
                t
            }
            Err(err) => {
                eprintln!("[orbit] packaged startup tasks: {err}");
                Vec::new()
            }
        },
    };
    build_entries(&tasks, &read_state)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MANIFEST: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<Package xmlns:uap5="x" xmlns:desktop="y">
  <Applications><Application Id="App">
    <Extensions>
      <!-- <uap5:StartupTask TaskId="Commented" Enabled="true" /> -->
      <uap5:Extension Category="windows.startupTask" Executable="WhatsApp.exe" EntryPoint="Windows.FullTrustApplication">
        <uap5:StartupTask TaskId="2defd21c-0b9e" Enabled="true" DisplayName="WhatsApp &amp; Co" />
      </uap5:Extension>
      <desktop:Extension Category="windows.startupTask" Executable='Claude.exe'>
        <desktop:StartupTask TaskId="ClaudeStartup" Enabled="false" DisplayName="ms-resource:AppName"/>
      </desktop:Extension>
      <uap:Extension Category="windows.protocol"><uap:Protocol Name="x"/></uap:Extension>
    </Extensions>
  </Application></Applications>
</Package>"#;

    #[test]
    fn manifest_tasks() {
        let t = parse_manifest(MANIFEST);
        assert_eq!(t.len(), 2, "{t:?}");
        assert_eq!(t[0].task_id, "2defd21c-0b9e");
        assert!(t[0].enabled);
        assert_eq!(t[0].display_name.as_deref(), Some("WhatsApp & Co"));
        assert_eq!(t[0].executable.as_deref(), Some("WhatsApp.exe"));
        assert_eq!(t[1].task_id, "ClaudeStartup");
        assert!(!t[1].enabled);
        assert_eq!(t[1].executable.as_deref(), Some("Claude.exe"));
        assert!(parse_manifest("<Package/>").is_empty());
        assert!(parse_manifest("<unclosed").is_empty());
    }

    #[test]
    fn states() {
        assert_eq!(state_name(Some(0), true), "disabled");
        assert_eq!(state_name(Some(1), true), "disabledByUser");
        assert_eq!(state_name(Some(2), false), "enabled");
        assert_eq!(state_name(Some(3), true), "disabledByPolicy");
        assert_eq!(state_name(Some(4), false), "enabledByPolicy");
        // Never touched: the manifest default.
        assert_eq!(state_name(None, true), "enabled");
        assert_eq!(state_name(None, false), "disabled");
        assert_eq!(state_name(Some(9), true), "enabled");
        assert!(is_enabled("enabled") && is_enabled("enabledByPolicy"));
        assert!(!is_enabled("disabled") && !is_enabled("disabledByUser") && !is_enabled("disabledByPolicy"));
    }

    #[test]
    fn entries_are_read_only_rows() {
        let task = |family: &str, id: &str, display: Option<&str>| PkgTask {
            family: family.into(),
            package_name: "Pkg".into(),
            task: ManifestTask {
                task_id: id.into(),
                enabled: false,
                display_name: display.map(Into::into),
                executable: None,
            },
        };
        let tasks = vec![
            task("A_1", "t1", Some("Zed")),
            task("B_2", "t2", Some("ms-resource:x")),
            task("B_2", "t3", Some("ms-resource:y")),
        ];
        let list = build_entries(&tasks, &|f, t| (f == "A_1" && t == "t1").then_some(2));
        assert_eq!(list.len(), 3);
        // Names that would repeat inside one package get the task id.
        let names: Vec<&str> = list.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["Pkg (t2)", "Pkg (t3)", "Zed"]);
        let zed = list.iter().find(|e| e.name == "Zed").unwrap();
        assert_eq!(zed.id, "Packaged\\A_1\\t1");
        assert!(zed.enabled && zed.packaged_state.as_deref() == Some("enabled"));
        assert_eq!(zed.location, "Store app");
        assert!(zed.path.is_none() && !zed.needs_admin && zed.impact.is_none());
        assert!(list.iter().filter(|e| e.name != "Zed").all(|e| !e.enabled));
    }

    #[test]
    fn packaged_ids_cannot_be_toggled() {
        let err = set_startup_enabled("Packaged\\Claude_pzs8sxrjxfjjc\\ClaudeStartup".into(), false).unwrap_err();
        assert!(err.contains("Startup settings"), "{err}");
    }

    /// Reads this PC's packages and registry (read only). Every row must be a
    /// well-formed packaged row.
    #[test]
    fn this_pc_lists_well_formed_rows() {
        for e in collect_packaged_startup(true) {
            assert!(e.id.starts_with(PREFIX) && e.id.matches('\\').count() == 2, "{}", e.id);
            assert!(!e.name.trim().is_empty());
            let st = e.packaged_state.as_deref().unwrap();
            assert_eq!(e.enabled, is_enabled(st));
        }
    }

    /// Prints this PC's rows and the time it takes: cargo test print_this_pc -- --ignored --nocapture
    #[test]
    #[ignore]
    fn print_this_pc() {
        let t = std::time::Instant::now();
        let list = collect_packaged_startup(true);
        println!("listed {} tasks in {:?}", list.len(), t.elapsed());
        for e in &list {
            println!("{:<28} {:<17} {}", e.name, e.packaged_state.as_deref().unwrap_or(""), e.id);
        }
        let t = std::time::Instant::now();
        let again = collect_packaged_startup(true);
        println!("again (manifests cached): {} in {:?}", again.len(), t.elapsed());
        let t = std::time::Instant::now();
        collect_packaged_startup(false);
        println!("states only: {:?}", t.elapsed());
    }
}
