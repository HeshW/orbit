//! Optimize > Debloat (0.6, feature 8f): reads the startup apps, scheduled
//! tasks, services and built-in apps on this PC, explains each one from a
//! reviewed list (`known.rs`) and turns off only the ones the user ticks.
//!
//! - Verdicts: Safe to turn off, Keep, or Unknown (not on the list: leave it).
//!   `guard.rs` refuses its never-offer list at analysis and again before every
//!   change, whatever the list or the UI says.
//! - Changes: startup apps are turned off (StartupApproved, as Task Manager),
//!   tasks are disabled, services are set to Manual (never Disabled, never
//!   stopped), built-in apps are removed for this user only. Nothing is deleted.
//! - Undo: every change is written to `debloat-changes.json` in Orbit's data
//!   folder with the previous state BEFORE it is made. Undo turns startup apps
//!   and tasks back on, restores a service's start type and opens the Store
//!   page of a removed app (reinstalling needs the Store).
//!
//! The real Windows calls live behind `Backend` (`system.rs`); the tests use a
//! fake, so they never change this PC.

pub mod guard;
pub mod known;
mod system;

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

use crate::health::now_secs;
use known::{Known, KNOWN};

const LOG_FILE: &str = "debloat-changes.json";

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[serde(rename_all = "camelCase")]
pub enum Group {
    Startup,
    Task,
    Service,
    App,
}

impl Group {
    fn prefix(self) -> &'static str {
        match self {
            Group::Startup => "startup:",
            Group::Task => "task:",
            Group::Service => "service:",
            Group::App => "app:",
        }
    }

    fn title(self) -> &'static str {
        match self {
            Group::Startup => "Startup apps",
            Group::Task => "Scheduled tasks",
            Group::Service => "Services",
            Group::App => "Built-in apps",
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum Verdict {
    /// Safe to turn off.
    Off,
    Keep,
    Unknown,
}

/// What the backend read from this PC.
#[derive(Clone, Debug, Default)]
pub struct Inventory {
    pub startup: Vec<RawStartup>,
    pub tasks: Vec<RawTask>,
    pub services: Vec<RawService>,
    pub apps: Vec<RawApp>,
    /// A part that could not be read ("Scheduled tasks: ..."); the rest still loads.
    pub errors: Vec<String>,
}

#[derive(Clone, Debug, Default)]
pub struct RawStartup {
    /// `StartupEntry.id` ("HKCU\\Run\\Steam").
    pub id: String,
    pub name: String,
    pub command: String,
    pub path: Option<String>,
    pub enabled: bool,
    pub needs_admin: bool,
    /// "high" | "medium" | "low" from Orbit's startup measurement.
    pub impact: Option<String>,
    /// Picked by the startup suggestions (health.rs `pick_suggestions`).
    pub suggested: bool,
}

#[derive(Clone, Debug, Default)]
pub struct RawTask {
    pub path: String,
    pub enabled: bool,
    pub exe: String,
    pub author: Option<String>,
    pub needs_admin: bool,
}

#[derive(Clone, Debug, Default)]
pub struct RawService {
    pub name: String,
    pub display: String,
    /// As in `ServiceEntry.start_type`.
    pub start_type: String,
    /// The binary it runs, "" if unknown.
    pub path: String,
    /// Starts by itself and runs a program outside the Windows folder.
    pub third_party: bool,
}

#[derive(Clone, Debug, Default)]
pub struct RawApp {
    /// Package name ("Microsoft.BingWeather").
    pub name: String,
    pub full_name: String,
    pub family: String,
    pub display: Option<String>,
    /// Framework, resource or system-signed package.
    pub system: bool,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Item {
    /// "startup:<StartupEntry.id>" | "task:<path>" | "service:<name>" | "app:<family>".
    pub id: String,
    pub group: Group,
    /// Plain name ("Microsoft Edge updater").
    pub name: String,
    /// The raw name Windows uses.
    pub raw: String,
    pub what: Option<String>,
    pub verdict: Verdict,
    /// What you lose (Safe to turn off) or why to keep it.
    pub note: Option<String>,
    /// On at sign-in / enabled / starts by itself / installed.
    pub on: bool,
    /// Safe to turn off, still on and not refused by the guard.
    pub actionable: bool,
    pub needs_admin: bool,
    /// What Orbit would do ("Turn off at sign-in"), None when not actionable.
    pub change: Option<String>,
    /// Command, program, start type or the app's own name.
    pub detail: Option<String>,
    pub impact: Option<String>,
    pub suggested: bool,
    /// Text for Search online.
    pub search: String,
    pub family: Option<String>,
    /// Refused by the never-offer list.
    pub guarded: bool,
}

#[derive(Serialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Counts {
    pub off: u32,
    pub keep: u32,
    pub unknown: u32,
    /// Safe to turn off and still on.
    pub ready: u32,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct GroupOut {
    pub id: Group,
    pub title: String,
    pub counts: Counts,
    pub items: Vec<Item>,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Analysis {
    pub read_at: u64,
    pub is_admin: bool,
    pub counts: Counts,
    pub groups: Vec<GroupOut>,
    pub errors: Vec<String>,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Planned {
    pub id: String,
    pub group: Group,
    pub name: String,
    pub raw: String,
    pub action: String,
    pub from: String,
    pub to: String,
    pub needs_admin: bool,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Refusal {
    pub id: String,
    pub name: String,
    pub reason: String,
    /// Refused only because Orbit is not running as administrator.
    pub needs_admin: bool,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Preview {
    pub changes: Vec<Planned>,
    pub refused: Vec<Refusal>,
    pub is_admin: bool,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum ChangeState {
    /// Written before the change was made; treated like applied.
    Pending,
    Applied,
    Undone,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Change {
    pub id: u64,
    /// `Item.id`.
    pub item: String,
    pub group: Group,
    pub name: String,
    pub raw: String,
    /// What Windows calls it: startup id, task path, service name or package full name.
    pub target: String,
    pub action: String,
    pub from: String,
    pub to: String,
    /// The state to restore: "enabled" (startup, task), the service start type
    /// ("auto" | "autoDelayed"), or "installed" (app).
    pub before: String,
    pub family: Option<String>,
    /// `ms-windows-store://pdp/?PFN=<family>` for a removed app.
    pub store_url: Option<String>,
    pub needs_admin: bool,
    pub at: u64,
    pub state: ChangeState,
    pub undone_at: Option<u64>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct ChangeLog {
    pub version: u32,
    pub next_id: u64,
    pub changes: Vec<Change>,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Failure {
    pub id: String,
    pub name: String,
    pub error: String,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ApplyResult {
    pub applied: Vec<Change>,
    pub failed: Vec<Failure>,
    pub refused: Vec<Refusal>,
    /// Every change, newest first.
    pub changes: Vec<Change>,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct UndoAllResult {
    pub undone: Vec<Change>,
    pub failed: Vec<Failure>,
    /// Removed apps still waiting: undo opens their Store page one at a time.
    pub store_apps: Vec<Change>,
    pub changes: Vec<Change>,
}

/// The Windows side. `system::Real` makes the real calls; the tests use a fake.
pub trait Backend {
    fn inventory(&self) -> Inventory;
    fn is_admin(&self) -> bool;
    fn set_startup(&self, id: &str, enabled: bool) -> Result<(), String>;
    fn set_task(&self, path: &str, enabled: bool) -> Result<(), String>;
    /// `start`: "manual" | "auto" | "autoDelayed".
    fn set_service_start(&self, name: &str, start: &str) -> Result<(), String>;
    /// Removes the package for the current user only.
    fn remove_app(&self, full_name: &str) -> Result<(), String>;
    fn app_installed(&self, family: &str) -> bool;
    fn open_store(&self, family: &str) -> Result<(), String>;
}

// ---------------------------------------------------------------------------
// Analysis
// ---------------------------------------------------------------------------

fn file_name(path: &str) -> &str {
    path.rsplit(['\\', '/']).next().unwrap_or(path)
}

fn start_label(start: &str) -> &'static str {
    match start {
        "auto" => "Automatic",
        "autoDelayed" => "Automatic (delayed)",
        "manual" => "Manual",
        "disabled" => "Disabled",
        "boot" | "system" => "At boot",
        _ => "Unknown",
    }
}

/// Last part of a task path, the name Task Scheduler shows.
fn task_leaf(path: &str) -> &str {
    path.rsplit('\\').find(|s| !s.is_empty()).unwrap_or(path)
}

struct Verdicted {
    verdict: Verdict,
    name: Option<String>,
    what: Option<String>,
    note: Option<String>,
    guarded: bool,
}

fn decide(known: Option<&Known>, refused: Option<&'static str>) -> Verdicted {
    match (known, refused) {
        // The guard wins over the list: a refused item is never Safe to turn off.
        (Some(k), Some(why)) if k.verdict == Verdict::Off => Verdicted {
            verdict: Verdict::Keep,
            name: Some(k.name.into()),
            what: Some(k.what.into()),
            note: Some(why.into()),
            guarded: true,
        },
        (Some(k), refused) => Verdicted {
            verdict: k.verdict,
            name: Some(k.name.into()),
            what: Some(k.what.into()),
            note: Some(k.note.into()),
            guarded: refused.is_some(),
        },
        (None, Some(why)) => Verdicted { verdict: Verdict::Keep, name: None, what: None, note: Some(why.into()), guarded: true },
        (None, None) => Verdicted { verdict: Verdict::Unknown, name: None, what: None, note: None, guarded: false },
    }
}

fn change_text(group: Group) -> &'static str {
    match group {
        Group::Startup => "Turn off at sign-in",
        Group::Task => "Turn off the task",
        Group::Service => "Set to Manual",
        Group::App => "Remove for this user",
    }
}

#[allow(clippy::too_many_arguments)]
fn item(
    group: Group,
    key: &str,
    raw: &str,
    fallback_name: &str,
    v: Verdicted,
    on: bool,
    needs_admin: bool,
    detail: Option<String>,
) -> Item {
    let actionable = v.verdict == Verdict::Off && on && !v.guarded;
    let name = v.name.unwrap_or_else(|| fallback_name.to_string());
    Item {
        id: format!("{}{key}", group.prefix()),
        group,
        search: if raw.is_empty() { name.clone() } else { raw.to_string() },
        name,
        raw: raw.to_string(),
        what: v.what,
        verdict: v.verdict,
        note: v.note,
        on,
        actionable,
        needs_admin,
        change: actionable.then(|| change_text(group).to_string()),
        detail,
        impact: None,
        suggested: false,
        family: None,
        guarded: v.guarded,
    }
}

fn startup_items(list: &[Known], inv: &Inventory) -> Vec<Item> {
    inv.startup
        .iter()
        .map(|e| {
            let file = e.path.as_deref().map(file_name).unwrap_or("");
            let keys: Vec<&str> = [e.name.as_str(), file].into_iter().filter(|k| !k.is_empty()).collect();
            let known = known::lookup(list, Group::Startup, &keys);
            let path = e.path.as_deref().unwrap_or(&e.command);
            let refused = guard::refuse(&guard::Subject {
                group: Group::Startup,
                key: &e.name,
                display: "",
                path,
                reviewed: known.is_some(),
                system_package: false,
            });
            let mut it = item(Group::Startup, &e.id, &e.name, &e.name, decide(known, refused), e.enabled, e.needs_admin, Some(e.command.clone()));
            it.search = if file.is_empty() { e.name.clone() } else { file.to_string() };
            it.impact = e.impact.clone();
            it.suggested = e.suggested && it.verdict == Verdict::Unknown;
            it
        })
        .collect()
}

fn task_items(list: &[Known], inv: &Inventory) -> Vec<Item> {
    inv.tasks
        .iter()
        .filter_map(|t| {
            let known = known::lookup(list, Group::Task, &[t.path.as_str()]);
            // Windows' own tasks are listed only when the reviewed list knows them.
            if known.is_none() && t.path.to_lowercase().starts_with("\\microsoft\\") {
                return None;
            }
            let refused = guard::refuse(&guard::Subject {
                group: Group::Task,
                key: &t.path,
                display: "",
                path: &t.exe,
                reviewed: known.is_some(),
                system_package: false,
            });
            let detail = if t.exe.is_empty() { t.author.clone() } else { Some(t.exe.clone()) };
            let mut it = item(Group::Task, &t.path, &t.path, task_leaf(&t.path), decide(known, refused), t.enabled, t.needs_admin, detail);
            it.search = task_leaf(&t.path).to_string();
            Some(it)
        })
        .collect()
}

fn service_items(list: &[Known], inv: &Inventory) -> Vec<Item> {
    inv.services
        .iter()
        .filter_map(|s| {
            let known = known::lookup(list, Group::Service, &[s.name.as_str()]);
            if known.is_none() && !s.third_party {
                return None;
            }
            let refused = guard::refuse(&guard::Subject {
                group: Group::Service,
                key: &s.name,
                display: &s.display,
                path: &s.path,
                reviewed: known.is_some(),
                system_package: false,
            });
            let on = s.start_type == "auto" || s.start_type == "autoDelayed";
            let detail = Some(format!("Starts: {}", start_label(&s.start_type)));
            let mut it = item(Group::Service, &s.name, &s.name, &s.display, decide(known, refused), on, true, detail);
            it.search = if s.display.is_empty() { s.name.clone() } else { s.display.clone() };
            Some(it)
        })
        .collect()
}

fn app_items(list: &[Known], inv: &Inventory) -> Vec<Item> {
    inv.apps
        .iter()
        .filter_map(|a| {
            // Built-in apps are listed only when the reviewed list knows them.
            let known = known::lookup(list, Group::App, &[a.name.as_str()])?;
            let refused = guard::refuse(&guard::Subject {
                group: Group::App,
                key: &a.name,
                display: a.display.as_deref().unwrap_or(""),
                path: "",
                reviewed: true,
                system_package: a.system,
            });
            let mut it = item(Group::App, &a.family, &a.name, &a.name, decide(Some(known), refused), true, false, a.display.clone());
            it.search = a.display.clone().unwrap_or_else(|| a.name.clone());
            it.family = Some(a.family.clone());
            Some(it)
        })
        .collect()
}

fn rank(it: &Item) -> u8 {
    match (it.verdict, it.actionable) {
        (Verdict::Off, true) => 0,
        (Verdict::Off, false) => 1,
        (Verdict::Keep, _) => 2,
        (Verdict::Unknown, _) => 3,
    }
}

fn count(items: &[Item]) -> Counts {
    let mut c = Counts::default();
    for it in items {
        match it.verdict {
            Verdict::Off => c.off += 1,
            Verdict::Keep => c.keep += 1,
            Verdict::Unknown => c.unknown += 1,
        }
        if it.actionable {
            c.ready += 1;
        }
    }
    c
}

/// Groups and verdicts for what `inv` holds, from `list` (the reviewed list in
/// the app, a test list in tests).
pub fn analyze(list: &[Known], inv: &Inventory, is_admin: bool) -> Analysis {
    let mut groups = Vec::new();
    let mut total = Counts::default();
    let built = [
        (Group::Startup, startup_items(list, inv)),
        (Group::Task, task_items(list, inv)),
        (Group::Service, service_items(list, inv)),
        (Group::App, app_items(list, inv)),
    ];
    for (group, mut items) in built {
        // One row per id (an app can match twice through its bundle).
        let mut seen = HashSet::new();
        items.retain(|it| seen.insert(it.id.clone()));
        items.sort_by_cached_key(|it| (rank(it), it.name.to_lowercase()));
        let c = count(&items);
        total.off += c.off;
        total.keep += c.keep;
        total.unknown += c.unknown;
        total.ready += c.ready;
        groups.push(GroupOut { id: group, title: group.title().into(), counts: c, items });
    }
    Analysis { read_at: now_secs(), is_admin, counts: total, groups, errors: inv.errors.clone() }
}

// ---------------------------------------------------------------------------
// Plan (preview) and apply
// ---------------------------------------------------------------------------

fn from_to(it: &Item, inv: &Inventory) -> (String, String, String) {
    match it.group {
        Group::Startup => ("On at sign-in".into(), "Off".into(), "enabled".into()),
        Group::Task => ("On".into(), "Off".into(), "enabled".into()),
        Group::Service => {
            let name = it.id.trim_start_matches(Group::Service.prefix());
            let start = inv
                .services
                .iter()
                .find(|s| s.name.eq_ignore_ascii_case(name))
                .map(|s| s.start_type.clone())
                .unwrap_or_else(|| "auto".into());
            (start_label(&start).into(), "Manual".into(), start)
        }
        Group::App => ("Installed".into(), "Removed for you".into(), "installed".into()),
    }
}

/// What would change for `ids`, and why the others won't. The guard is checked
/// again here, whatever the analysis said.
fn plan(list: &[Known], analysis: &Analysis, inv: &Inventory, ids: &[String]) -> (Vec<(Item, Planned, String)>, Vec<Refusal>) {
    let mut ok = Vec::new();
    let mut refused = Vec::new();
    let mut seen = HashSet::new();
    for id in ids {
        if !seen.insert(id.clone()) {
            continue;
        }
        let found = analysis.groups.iter().flat_map(|g| g.items.iter()).find(|it| &it.id == id);
        let Some(it) = found else {
            refused.push(Refusal { id: id.clone(), name: id.clone(), reason: "It is no longer on this PC. Analyze again.".into(), needs_admin: false });
            continue;
        };
        let deny = |reason: &str, needs_admin: bool| Refusal { id: id.clone(), name: it.name.clone(), reason: reason.into(), needs_admin };
        if let Some(why) = recheck_guard(list, it, inv) {
            refused.push(deny(why, false));
            continue;
        }
        if it.verdict != Verdict::Off {
            refused.push(deny("Orbit only changes items marked Safe to turn off.", false));
            continue;
        }
        if !it.on {
            refused.push(deny("It is already off.", false));
            continue;
        }
        if it.needs_admin && !analysis.is_admin {
            refused.push(deny("Needs administrator rights.", true));
            continue;
        }
        let (from, to, before) = from_to(it, inv);
        let planned = Planned {
            id: it.id.clone(),
            group: it.group,
            name: it.name.clone(),
            raw: it.raw.clone(),
            action: change_text(it.group).into(),
            from,
            to,
            needs_admin: it.needs_admin,
        };
        ok.push((it.clone(), planned, before));
    }
    (ok, refused)
}

/// The guard again, from the raw item and a fresh list lookup (not the
/// analysis's verdict). An item that can't be found is refused.
fn recheck_guard(list: &[Known], it: &Item, inv: &Inventory) -> Option<&'static str> {
    const GONE: &str = "It is no longer on this PC. Analyze again.";
    let key = &it.id[it.group.prefix().len()..];
    let check = |keys: &[&str], k: &str, display: &str, path: &str, system: bool| {
        guard::refuse(&guard::Subject {
            group: it.group,
            key: k,
            display,
            path,
            reviewed: known::lookup(list, it.group, keys).is_some(),
            system_package: system,
        })
    };
    match it.group {
        Group::Startup => inv.startup.iter().find(|e| e.id == key).map_or(Some(GONE), |e| {
            let path = e.path.as_deref().unwrap_or(&e.command);
            check(&[e.name.as_str(), file_name(path)], &e.name, "", path, false)
        }),
        Group::Task => inv.tasks.iter().find(|t| t.path == key).map_or(Some(GONE), |t| check(&[t.path.as_str()], &t.path, "", &t.exe, false)),
        Group::Service => inv
            .services
            .iter()
            .find(|s| s.name == key)
            .map_or(Some(GONE), |s| check(&[s.name.as_str()], &s.name, &s.display, &s.path, false)),
        Group::App => inv.apps.iter().find(|a| a.family == key).map_or(Some(GONE), |a| check(&[a.name.as_str()], &a.name, "", "", a.system)),
    }
}

pub fn preview_with(b: &dyn Backend, list: &[Known], ids: &[String]) -> Preview {
    let inv = b.inventory();
    let admin = b.is_admin();
    let analysis = analyze(list, &inv, admin);
    let (ok, refused) = plan(list, &analysis, &inv, ids);
    Preview { changes: ok.into_iter().map(|(_, p, _)| p).collect(), refused, is_admin: admin }
}

fn target_of(it: &Item, inv: &Inventory) -> String {
    let key = &it.id[it.group.prefix().len()..];
    match it.group {
        Group::App => inv.apps.iter().find(|a| a.family == key).map(|a| a.full_name.clone()).unwrap_or_default(),
        _ => key.to_string(),
    }
}

fn store_url(family: &str) -> String {
    format!("ms-windows-store://pdp/?PFN={family}")
}

/// A package family name as Windows writes it ("Name_publisherid").
pub fn valid_family(family: &str) -> bool {
    !family.is_empty()
        && family.len() <= 128
        && family.matches('_').count() == 1
        && !family.starts_with('_')
        && !family.ends_with('_')
        && family.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_')
}

pub fn apply_with(b: &dyn Backend, list: &[Known], dir: &Path, ids: &[String]) -> Result<ApplyResult, String> {
    let _lock = lock();
    let inv = b.inventory();
    let analysis = analyze(list, &inv, b.is_admin());
    let (ok, refused) = plan(list, &analysis, &inv, ids);
    let mut log = load_log(dir)?;
    let mut applied = Vec::new();
    let mut failed = Vec::new();
    for (it, p, before) in ok {
        let target = target_of(&it, &inv);
        if target.is_empty() {
            failed.push(Failure { id: it.id.clone(), name: it.name.clone(), error: "It is no longer on this PC. Analyze again.".into() });
            continue;
        }
        log.next_id = log.next_id.max(1);
        let change = Change {
            id: log.next_id,
            item: it.id.clone(),
            group: it.group,
            name: it.name.clone(),
            raw: it.raw.clone(),
            target: target.clone(),
            action: p.action.clone(),
            from: p.from.clone(),
            to: p.to.clone(),
            before,
            family: it.family.clone(),
            store_url: it.family.as_deref().map(store_url),
            needs_admin: it.needs_admin,
            at: now_secs(),
            state: ChangeState::Pending,
            undone_at: None,
        };
        log.next_id += 1;
        // Saved before anything changes, so Undo knows the previous state even
        // if Orbit stops halfway.
        log.changes.push(change.clone());
        if let Err(e) = save_log(dir, &log) {
            log.changes.pop();
            failed.push(Failure { id: it.id.clone(), name: it.name.clone(), error: format!("Orbit could not save its change list, so it changed nothing ({e}).") });
            break;
        }
        let result = match it.group {
            Group::Startup => b.set_startup(&target, false),
            Group::Task => b.set_task(&target, false),
            Group::Service => b.set_service_start(&target, "manual"),
            Group::App => b.remove_app(&target),
        };
        match result {
            Ok(()) => {
                let last = log.changes.last_mut().expect("just pushed");
                last.state = ChangeState::Applied;
                applied.push(last.clone());
            }
            Err(e) => {
                // Nothing changed, so nothing to undo.
                log.changes.pop();
                failed.push(Failure { id: it.id.clone(), name: it.name.clone(), error: e });
            }
        }
        save_log(dir, &log)?;
    }
    Ok(ApplyResult { applied, failed, refused, changes: newest_first(&log) })
}

// ---------------------------------------------------------------------------
// Change log and undo
// ---------------------------------------------------------------------------

static LOCK: Mutex<()> = Mutex::new(());

fn lock() -> std::sync::MutexGuard<'static, ()> {
    LOCK.lock().unwrap_or_else(|p| p.into_inner())
}

pub fn load_log(dir: &Path) -> Result<ChangeLog, String> {
    let path = dir.join(LOG_FILE);
    match fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| format!("Orbit's change list could not be read ({e}).")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(ChangeLog { version: 1, next_id: 1, changes: Vec::new() }),
        Err(e) => Err(format!("Orbit's change list could not be read ({e}).")),
    }
}

/// Written to a temporary file, then renamed over the log.
pub fn save_log(dir: &Path, log: &ChangeLog) -> Result<(), String> {
    fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let bytes = serde_json::to_vec_pretty(log).map_err(|e| e.to_string())?;
    let path = dir.join(LOG_FILE);
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, bytes).map_err(|e| e.to_string())?;
    fs::rename(&tmp, &path).map_err(|e| e.to_string())
}

fn newest_first(log: &ChangeLog) -> Vec<Change> {
    let mut v = log.changes.clone();
    v.reverse();
    v
}

fn is_live(c: &Change) -> bool {
    c.state != ChangeState::Undone
}

/// Every change, newest first. A removed app that is installed again counts as undone.
pub fn changes_with(b: &dyn Backend, dir: &Path) -> Result<Vec<Change>, String> {
    let _lock = lock();
    let mut log = load_log(dir)?;
    let mut dirty = false;
    for c in log.changes.iter_mut().filter(|c| c.group == Group::App && is_live(c)) {
        // Pending too: an app whose removal never finished (Orbit stopped) is still installed.
        if c.family.as_deref().is_some_and(|f| b.app_installed(f)) {
            c.state = ChangeState::Undone;
            c.undone_at = Some(now_secs());
            dirty = true;
        }
    }
    if dirty {
        save_log(dir, &log)?;
    }
    Ok(newest_first(&log))
}

fn undo_one(b: &dyn Backend, c: &Change) -> Result<bool, String> {
    if c.needs_admin && c.group != Group::App && !b.is_admin() {
        return Err("Needs administrator rights. Restart Orbit as administrator, then undo.".into());
    }
    match c.group {
        Group::Startup => b.set_startup(&c.target, true).map(|_| true),
        Group::Task => b.set_task(&c.target, true).map(|_| true),
        Group::Service => {
            let start = if c.before == "autoDelayed" { "autoDelayed" } else { "auto" };
            b.set_service_start(&c.target, start).map(|_| true)
        }
        // Reinstalling needs the Store: open the app's page. It counts as undone
        // once the app is back (changes_with checks).
        Group::App => {
            let family = c.family.as_deref().filter(|f| valid_family(f)).ok_or("Orbit doesn't know this app's Store page.")?;
            b.open_store(family).map(|_| false)
        }
    }
}

pub fn undo_with(b: &dyn Backend, dir: &Path, id: u64) -> Result<Change, String> {
    let _lock = lock();
    let mut log = load_log(dir)?;
    let idx = log.changes.iter().position(|c| c.id == id).ok_or("Orbit has no record of that change.")?;
    if !is_live(&log.changes[idx]) {
        return Err("That change was already undone.".into());
    }
    let done = undo_one(b, &log.changes[idx])?;
    if done {
        log.changes[idx].state = ChangeState::Undone;
        log.changes[idx].undone_at = Some(now_secs());
        save_log(dir, &log)?;
    }
    Ok(log.changes[idx].clone())
}

/// Undoes every live change, newest first. Removed apps are left for the user:
/// each one needs its own Store page.
pub fn undo_all_with(b: &dyn Backend, dir: &Path) -> Result<UndoAllResult, String> {
    let _lock = lock();
    let mut log = load_log(dir)?;
    let mut undone = Vec::new();
    let mut failed = Vec::new();
    let mut store_apps = Vec::new();
    for i in (0..log.changes.len()).rev() {
        if !is_live(&log.changes[i]) {
            continue;
        }
        if log.changes[i].group == Group::App {
            store_apps.push(log.changes[i].clone());
            continue;
        }
        match undo_one(b, &log.changes[i]) {
            Ok(_) => {
                log.changes[i].state = ChangeState::Undone;
                log.changes[i].undone_at = Some(now_secs());
                undone.push(log.changes[i].clone());
            }
            Err(e) => failed.push(Failure { id: log.changes[i].item.clone(), name: log.changes[i].name.clone(), error: e }),
        }
    }
    save_log(dir, &log)?;
    Ok(UndoAllResult { undone, failed, store_apps, changes: newest_first(&log) })
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

/// `<app local data>\debloat`; debug builds honour ORBIT_DEBLOAT_DIR (tests).
fn data_dir(app: &AppHandle) -> Result<PathBuf, String> {
    #[cfg(debug_assertions)]
    if let Some(v) = std::env::var_os("ORBIT_DEBLOAT_DIR").filter(|v| !v.is_empty()) {
        return Ok(PathBuf::from(v));
    }
    app.path()
        .app_local_data_dir()
        .map(|d| d.join("debloat"))
        .map_err(|e| format!("Could not find Orbit's data folder ({e})."))
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Result<T, String> + Send + 'static) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(f).await.map_err(|e| format!("Debloat failed: {e}"))?
}

fn check_ids(ids: &[String]) -> Result<(), String> {
    if ids.is_empty() {
        return Err("Tick at least one item first.".into());
    }
    if ids.len() > 500 || ids.iter().any(|i| i.len() > 1024) {
        return Err("That list is too long.".into());
    }
    Ok(())
}

#[tauri::command]
pub async fn debloat_analyze() -> Result<Analysis, String> {
    blocking(|| {
        let b = system::Real;
        let inv = b.inventory();
        Ok(analyze(KNOWN, &inv, b.is_admin()))
    })
    .await
}

#[tauri::command]
pub async fn debloat_preview(ids: Vec<String>) -> Result<Preview, String> {
    check_ids(&ids)?;
    blocking(move || Ok(preview_with(&system::Real, KNOWN, &ids))).await
}

#[tauri::command]
pub async fn debloat_apply(app: AppHandle, ids: Vec<String>) -> Result<ApplyResult, String> {
    check_ids(&ids)?;
    let dir = data_dir(&app)?;
    blocking(move || apply_with(&system::Real, KNOWN, &dir, &ids)).await
}

#[tauri::command]
pub async fn debloat_changes(app: AppHandle) -> Result<Vec<Change>, String> {
    let dir = data_dir(&app)?;
    blocking(move || changes_with(&system::Real, &dir)).await
}

#[tauri::command]
pub async fn debloat_undo(app: AppHandle, id: u64) -> Result<Change, String> {
    let dir = data_dir(&app)?;
    blocking(move || undo_with(&system::Real, &dir, id)).await
}

#[tauri::command]
pub async fn debloat_undo_all(app: AppHandle) -> Result<UndoAllResult, String> {
    let dir = data_dir(&app)?;
    blocking(move || undo_all_with(&system::Real, &dir)).await
}

#[cfg(test)]
mod tests;
