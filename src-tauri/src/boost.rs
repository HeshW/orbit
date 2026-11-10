//! Game / focus boost (v0.5, 7d), Optimize > Boost.
//!
//! While Boost is on, the user's chosen background apps get Below normal
//! priority (or, with the pause option, are suspended) and the power plan
//! switches to Ultimate or High performance. Turning it off puts back each
//! process's own priority, resumes what was paused and restores the plan.
//!
//! Safety:
//! - Targets are only processes whose exe name is on the user's list, in the
//!   user's own session (never session 0, so never services), not grouped as
//!   Windows, not a listed game, not Orbit. Each one then goes through the
//!   same guards as the Processes view (`actions::guard_pid`, which also
//!   covers Orbit's WebView2 helpers, and `actions::refuse_critical_suspend`).
//! - Priority is only lowered from Normal, Above normal or High. Idle and
//!   Below normal stay; Realtime is never touched.
//! - What Boost will change is written to `boost-state.json` *before* the
//!   first change. If Orbit crashes, the next start restores from that file
//!   (`init`). `crate::exit_cleanup` restores on a normal exit (`on_exit`).
//! - Restore checks each pid against its start time, so a pid that was reused
//!   is never touched; processes that exited are counted and skipped quietly.
//!   A process whose priority the user changed during Boost is left as it is,
//!   and so is a power plan the user switched away from.
//!
//! Auto mode needs no extra thread: the sampler calls `on_snapshot` after
//! each tick, and Boost turns on when a listed game is in the process list
//! and off when the last one is gone.
//!
//! Files live in `ORBIT_BOOST_DIR` when set (tests), else
//! `<app local data>\boost`: `boost-config.json` (the lists and toggles the
//! UI last sent, so auto mode works before the Boost tab is opened) and
//! `boost-state.json` (only while Boost is on).

use std::collections::{BTreeMap, HashSet};
use std::ffi::c_void;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager};
use windows::core::{w, GUID, HRESULT};
use windows::Win32::Foundation::{LocalFree, ERROR_CANCELLED, HLOCAL, HWND};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_INPROC_SERVER,
    COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE,
};
use windows::Win32::System::Power::{
    PowerEnumerate, PowerGetActiveScheme, PowerReadFriendlyName, PowerSetActiveScheme,
    ACCESS_SCHEME,
};
use windows::Win32::System::Registry::HKEY;
use windows::Win32::System::RemoteDesktop::ProcessIdToSessionId;
use windows::Win32::System::Threading::{
    GetPriorityClass, SetPriorityClass, ABOVE_NORMAL_PRIORITY_CLASS, BELOW_NORMAL_PRIORITY_CLASS,
    HIGH_PRIORITY_CLASS, NORMAL_PRIORITY_CLASS, PROCESS_CREATION_FLAGS,
    PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SET_INFORMATION, PROCESS_SUSPEND_RESUME,
};
use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
use windows::Win32::UI::Shell::{
    FileOpenDialog, IFileOpenDialog, FOS_FILEMUSTEXIST, FOS_FORCEFILESYSTEM, FOS_NOCHANGEDIR,
    SIGDN_FILESYSPATH,
};

use crate::actions::{self, EXITED};
use crate::model::{ProcessKind, ProcessRow, Snapshot, SuspendedSet};
use crate::sampler::AppState;

/// Sent with a `BoostStatus` whenever Boost turns on or off, its config
/// changes, or the set of running games changes.
pub const BOOST_EVENT: &str = "orbit://boost";
const STATE_FILE: &str = "boost-state.json";
const CONFIG_FILE: &str = "boost-config.json";
const MAX_NAMES: usize = 64;

/// Built-in power scheme GUIDs. Ultimate Performance is usually added as a
/// copy with a new GUID, so plans are also matched by name.
const ULTIMATE: u128 = 0xe9a42b02_d5df_448d_aa00_03f14749eb61;
const HIGH_PERFORMANCE: u128 = 0x8c5e7fda_e8bf_4a96_9a85_a6e23a8c635c;

const BELOW: u32 = BELOW_NORMAL_PRIORITY_CLASS.0;

const NO_PLAN: &str = "This PC has no High performance plan, so the power plan stays as it is.";
const PLAN_CHANGED: &str =
    "The power plan was changed while Boost was on, so Orbit left the new one.";

/// True while Boost is on (read by the tray tooltip).
static ACTIVE: AtomicBool = AtomicBool::new(false);

pub fn is_active() -> bool {
    ACTIVE.load(Ordering::Relaxed)
}

// ---------------------------------------------------------------------------
// Shapes
// ---------------------------------------------------------------------------

/// The lists and toggles from the UI (settings `boostGames`, `boostApps`,
/// `boostAuto`, `boostPause`, `boostPower`).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct BoostConfig {
    /// Exe names ("game.exe") that turn Boost on in auto mode.
    pub games: Vec<String>,
    /// Exe names of the apps Boost lowers or pauses.
    pub apps: Vec<String>,
    pub auto: bool,
    /// Pause (suspend) the apps instead of lowering their priority.
    pub pause: bool,
    /// Switch to Ultimate / High performance.
    pub power: bool,
}

impl Default for BoostConfig {
    fn default() -> Self {
        Self { games: Vec::new(), apps: Vec::new(), auto: false, pause: false, power: true }
    }
}

/// "C:\Games\Game.exe" or "Game.exe" -> "Game.exe". None for empty names.
fn clean_name(raw: &str) -> Option<String> {
    let s = raw.trim().trim_matches('"').trim();
    let name = s.rsplit(['\\', '/']).next().unwrap_or("").trim();
    if name.is_empty() || name.len() > 260 || name.chars().any(|c| c.is_control()) {
        return None;
    }
    Some(name.to_string())
}

fn clean_list(list: &[String]) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for raw in list {
        if let Some(name) = clean_name(raw) {
            if seen.insert(name.to_lowercase()) {
                out.push(name);
            }
        }
        if out.len() == MAX_NAMES {
            break;
        }
    }
    out
}

impl BoostConfig {
    fn sanitized(&self) -> Self {
        Self { games: clean_list(&self.games), apps: clean_list(&self.apps), ..self.clone() }
    }
}

/// One process Boost changed. `paused` false: priority lowered to Below
/// normal from `before` (a priority class value).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
struct Touched {
    pid: u32,
    /// Unix seconds, as in `ProcessRow::start_time`.
    start: u64,
    name: String,
    paused: bool,
    /// Priority class before Boost (0 when unknown; only used for lowered).
    before: u32,
}

/// The "before" state, written to `boost-state.json` while Boost is on.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
struct Saved {
    started_at: u64,
    /// "manual" or "auto".
    reason: String,
    /// The game that started an auto boost.
    trigger: Option<String>,
    /// Power plan GUIDs: the one before Boost and the one Boost switched to.
    power_before: Option<String>,
    power_set: Option<String>,
    procs: Vec<Touched>,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ProcNote {
    pub pid: u32,
    pub name: String,
    pub reason: String,
}

#[derive(Serialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RestoreReport {
    pub at: u64,
    /// Processes put back (priority restored or resumed).
    pub restored: u32,
    /// Processes that had exited (skipped).
    pub exited: u32,
    /// Processes whose priority the user changed during Boost (left as is).
    pub left_alone: u32,
    pub failed: Vec<ProcNote>,
    /// "none" (Boost did not switch it) | "restored" | "left" | "failed".
    pub power: String,
    pub power_note: Option<String>,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Plan {
    pub guid: String,
    pub name: String,
}

#[derive(Serialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PowerStatus {
    pub current: Option<Plan>,
    /// The plan Boost switches to, if this PC has one.
    pub target: Option<Plan>,
    /// While Boost is on and switched the plan: the plan it will restore.
    pub before: Option<Plan>,
    pub note: Option<String>,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Changed {
    pub pid: u32,
    pub name: String,
    /// "lowered" | "paused".
    pub action: String,
    /// Priority before Boost ("normal", "aboveNormal", "high").
    pub before: Option<String>,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct BoostStatus {
    pub active: bool,
    pub reason: Option<String>,
    pub trigger: Option<String>,
    pub since: Option<u64>,
    pub config: BoostConfig,
    pub power: PowerStatus,
    pub changed: Vec<Changed>,
    pub skipped: Vec<ProcNote>,
    /// Listed games running now (names as the process list shows them).
    pub running_games: Vec<String>,
    pub last_restore: Option<RestoreReport>,
    /// Set when this start restored a boost left on by a crash.
    pub recovered: Option<RestoreReport>,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Suggestion {
    pub name: String,
    pub exe: Option<String>,
    /// Summed over the app's processes: CPU percent and working set bytes.
    pub cpu: f64,
    pub memory: u64,
    pub count: u32,
    pub has_window: bool,
}

// ---------------------------------------------------------------------------
// OS seams (faked in tests)
// ---------------------------------------------------------------------------

/// Process calls. Every call checks the pid against `start` (Unix seconds)
/// and returns `actions::EXITED` when the process is gone or the pid now
/// belongs to another process.
pub(crate) trait ProcOps {
    /// May Boost touch this process at all (Orbit, System, critical, ...)?
    fn check(&self, pid: u32, start: u64) -> Result<(), String>;
    fn priority(&self, pid: u32, start: u64) -> Result<u32, String>;
    fn set_priority(&self, pid: u32, start: u64, class: u32) -> Result<(), String>;
    fn suspend(&self, pid: u32, start: u64) -> Result<(), String>;
    fn resume(&self, pid: u32, start: u64) -> Result<(), String>;
}

pub(crate) trait PowerOps {
    fn active(&self) -> Result<u128, String>;
    fn schemes(&self) -> Vec<(u128, String)>;
    fn set_active(&self, guid: u128) -> Result<(), String>;
}

// ---------------------------------------------------------------------------
// Pure logic
// ---------------------------------------------------------------------------

fn guid_string(g: u128) -> String {
    let h = format!("{g:032x}");
    format!("{}-{}-{}-{}-{}", &h[0..8], &h[8..12], &h[12..16], &h[16..20], &h[20..32])
}

fn parse_guid(s: &str) -> Option<u128> {
    let hex: String = s.trim().trim_matches(['{', '}']).chars().filter(|&c| c != '-').collect();
    if hex.len() != 32 {
        return None;
    }
    u128::from_str_radix(&hex, 16).ok()
}

/// Ultimate Performance first, then High performance; by GUID or English name.
fn choose_target(schemes: &[(u128, String)]) -> Option<&(u128, String)> {
    let named = |s: &(u128, String), n: &str| s.1.trim().eq_ignore_ascii_case(n);
    schemes
        .iter()
        .find(|s| s.0 == ULTIMATE || named(s, "Ultimate Performance"))
        .or_else(|| schemes.iter().find(|s| s.0 == HIGH_PERFORMANCE || named(s, "High performance")))
}

fn name_in(list: &[String], name: &str) -> bool {
    list.iter().any(|n| n.eq_ignore_ascii_case(name))
}

/// Who Orbit is: never a target, and the session targets must share.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Context {
    own_pid: u32,
    own_session: Option<u32>,
}

/// True for an exe in the Windows folder (explorer.exe with a window is
/// grouped as an app, but it is still Windows).
fn in_windows_dir(exe: &str) -> bool {
    static ROOT: OnceLock<String> = OnceLock::new();
    let root = ROOT.get_or_init(|| {
        let r = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".to_string());
        format!("{}\\", actions::normalize_path(&r).trim_end_matches('\\'))
    });
    actions::normalize_path(exe).starts_with(root.as_str())
}

impl Context {
    fn current() -> Self {
        let own_pid = std::process::id();
        let mut session = 0u32;
        let own_session = unsafe { ProcessIdToSessionId(own_pid, &mut session) }.ok().map(|_| session);
        Self { own_pid, own_session }
    }
}

/// The cheap, row-only half of the target rules (the OS half is
/// `ProcOps::check`): the user's own session but never session 0, not part
/// of Windows, not a listed game, not Orbit.
fn may_target(row: &ProcessRow, cfg: &BoostConfig, cx: &Context) -> bool {
    let session_ok = row
        .session_id
        .is_some_and(|s| s != 0 && cx.own_session.map_or(true, |own| own == s));
    row.pid != 0
        && row.pid != 4
        && row.pid != cx.own_pid
        && row.kind != ProcessKind::Windows
        && !row.exe.as_deref().is_some_and(in_windows_dir)
        && session_ok
        && !name_in(&cfg.games, &row.name)
        && !row.name.eq_ignore_ascii_case("orbit.exe")
}

fn pick_targets<'a>(rows: &'a [ProcessRow], cfg: &BoostConfig, cx: &Context) -> Vec<&'a ProcessRow> {
    rows.iter()
        .filter(|r| name_in(&cfg.apps, &r.name) && may_target(r, cfg, cx))
        .collect()
}

/// Names of the listed games in `rows`, sorted, once each.
fn running_games(rows: &[ProcessRow], games: &[String]) -> Vec<String> {
    if games.is_empty() {
        return Vec::new();
    }
    let mut out: BTreeMap<String, String> = BTreeMap::new();
    for r in rows.iter().filter(|r| name_in(games, &r.name)) {
        out.entry(r.name.to_lowercase()).or_insert_with(|| r.name.clone());
    }
    out.into_values().collect()
}

/// The heaviest apps in the user's session by CPU and memory, grouped by exe
/// name: candidates for the list. Orbit's WebView2 helpers are left out.
fn suggestions(rows: &[ProcessRow], cfg: &BoostConfig, cx: &Context) -> Vec<Suggestion> {
    let mut groups: BTreeMap<String, Suggestion> = BTreeMap::new();
    for r in rows {
        if !may_target(r, cfg, cx)
            || r.parent_pid == Some(cx.own_pid)
            || r.name.eq_ignore_ascii_case("msedgewebview2.exe")
        {
            continue;
        }
        let g = groups.entry(r.name.to_lowercase()).or_insert_with(|| Suggestion {
            name: r.name.clone(),
            exe: r.exe.clone(),
            cpu: 0.0,
            memory: 0,
            count: 0,
            has_window: false,
        });
        g.cpu += r.cpu;
        g.memory += r.memory;
        g.count += 1;
        g.has_window |= r.kind == ProcessKind::App;
        if g.exe.is_none() {
            g.exe = r.exe.clone();
        }
    }
    let score = |s: &Suggestion| s.cpu + s.memory as f64 / (100.0 * 1024.0 * 1024.0);
    let mut out: Vec<Suggestion> = groups
        .into_values()
        .filter(|s| s.cpu >= 1.0 || s.memory >= 200 * 1024 * 1024)
        .map(|mut s| {
            s.cpu = (s.cpu * 10.0).round() / 10.0;
            s
        })
        .collect();
    out.sort_by(|a, b| score(b).total_cmp(&score(a)).then_with(|| a.name.cmp(&b.name)));
    out.truncate(8);
    out
}

fn lowerable(class: u32) -> bool {
    class == NORMAL_PRIORITY_CLASS.0 || class == ABOVE_NORMAL_PRIORITY_CLASS.0 || class == HIGH_PRIORITY_CLASS.0
}

fn note(row_pid: u32, name: &str, reason: impl Into<String>) -> ProcNote {
    ProcNote { pid: row_pid, name: name.to_string(), reason: reason.into() }
}

/// What Boost would change for `targets`, reading each one's priority now.
/// Refusals go to `skipped`; processes that exited are dropped quietly.
fn plan_procs(targets: &[&ProcessRow], pause: bool, ops: &dyn ProcOps, skipped: &mut Vec<ProcNote>) -> Vec<Touched> {
    let mut out = Vec::new();
    for row in targets {
        let (pid, start) = (row.pid, row.start_time);
        if let Err(e) = ops.check(pid, start) {
            if e != EXITED {
                skipped.push(note(pid, &row.name, e));
            }
            continue;
        }
        if row.suspended {
            skipped.push(note(pid, &row.name, "Already paused in Orbit."));
            continue;
        }
        match ops.priority(pid, start) {
            Ok(class) if pause || lowerable(class) => {
                out.push(Touched { pid, start, name: row.name.clone(), paused: pause, before: class });
            }
            Ok(_) => skipped.push(note(pid, &row.name, "Already at a low priority.")),
            Err(e) if e == EXITED => {}
            // The priority is only informational when pausing.
            Err(_) if pause => {
                out.push(Touched { pid, start, name: row.name.clone(), paused: true, before: 0 });
            }
            Err(e) => skipped.push(note(pid, &row.name, e)),
        }
    }
    out
}

/// Applies `planned`; keeps only what took effect.
fn apply_procs(planned: Vec<Touched>, ops: &dyn ProcOps, skipped: &mut Vec<ProcNote>) -> Vec<Touched> {
    planned
        .into_iter()
        .filter(|t| {
            let result = if t.paused {
                ops.suspend(t.pid, t.start)
            } else {
                ops.set_priority(t.pid, t.start, BELOW)
            };
            match result {
                Ok(()) => true,
                Err(e) => {
                    if e != EXITED {
                        skipped.push(note(t.pid, &t.name, e));
                    }
                    false
                }
            }
        })
        .collect()
}

/// (plan before, plan to switch to), None when already on it. Err is a note
/// for the UI (no target plan, or the active plan could not be read).
fn plan_power(power: &dyn PowerOps) -> Result<Option<(u128, u128)>, String> {
    let active = power.active().map_err(|e| format!("Could not read the power plan: {e}"))?;
    let schemes = power.schemes();
    let target = choose_target(&schemes).ok_or_else(|| NO_PLAN.to_string())?;
    Ok((target.0 != active).then_some((active, target.0)))
}

struct Started {
    saved: Saved,
    skipped: Vec<ProcNote>,
    power_note: Option<String>,
}

/// Turns Boost on: plans every change, saves the plan with `save` (nothing
/// is changed if that fails), applies it, then saves what took effect.
#[allow(clippy::too_many_arguments)]
fn start_boost(
    cfg: &BoostConfig,
    rows: &[ProcessRow],
    cx: &Context,
    reason: &str,
    trigger: Option<String>,
    now: u64,
    ops: &dyn ProcOps,
    power: &dyn PowerOps,
    save: &mut dyn FnMut(&Saved) -> Result<(), String>,
) -> Result<Started, String> {
    let mut skipped = Vec::new();
    let targets = pick_targets(rows, cfg, cx);
    let planned = plan_procs(&targets, cfg.pause, ops, &mut skipped);
    let mut saved = Saved {
        started_at: now,
        reason: reason.to_string(),
        trigger,
        procs: planned.clone(),
        ..Default::default()
    };
    let mut power_note = None;
    let mut switch_to = None;
    if cfg.power {
        match plan_power(power) {
            Ok(Some((before, target))) => {
                saved.power_before = Some(guid_string(before));
                saved.power_set = Some(guid_string(target));
                switch_to = Some(target);
            }
            Ok(None) => {}
            Err(n) => power_note = Some(n),
        }
    }
    save(&saved).map_err(|e| format!("Boost did not start: Orbit could not save what it would change ({e})."))?;

    if let Some(target) = switch_to {
        if let Err(e) = power.set_active(target) {
            power_note = Some(format!("Could not switch the power plan: {e}"));
            saved.power_before = None;
            saved.power_set = None;
        }
    }
    saved.procs = apply_procs(planned, ops, &mut skipped);
    // If this fails the first file is still there; it only lists more.
    let _ = save(&saved);
    Ok(Started { saved, skipped, power_note })
}

/// Puts back everything in `saved`.
fn restore_boost(saved: &Saved, now: u64, ops: &dyn ProcOps, power: &dyn PowerOps) -> RestoreReport {
    let mut r = RestoreReport { at: now, power: "none".into(), ..Default::default() };
    for t in &saved.procs {
        let result = if t.paused {
            ops.resume(t.pid, t.start).map(|_| true)
        } else {
            match ops.priority(t.pid, t.start) {
                Ok(class) if class != BELOW => Ok(false),
                Ok(_) => ops.set_priority(t.pid, t.start, t.before).map(|_| true),
                Err(e) => Err(e),
            }
        };
        match result {
            Ok(true) => r.restored += 1,
            Ok(false) => r.left_alone += 1,
            Err(e) if e == EXITED => r.exited += 1,
            Err(e) => r.failed.push(note(t.pid, &t.name, e)),
        }
    }
    let before = saved.power_before.as_deref().and_then(parse_guid);
    let set = saved.power_set.as_deref().and_then(parse_guid);
    if let (Some(before), Some(set)) = (before, set) {
        match power.active() {
            Ok(current) if current != set => {
                r.power = "left".into();
                r.power_note = Some(PLAN_CHANGED.into());
            }
            // Unreadable: put the old plan back anyway.
            _ => match power.set_active(before) {
                Ok(()) => r.power = "restored".into(),
                Err(e) => {
                    r.power = "failed".into();
                    r.power_note = Some(format!("Could not restore the power plan: {e}"));
                }
            },
        }
    }
    r
}

#[derive(Debug, PartialEq)]
enum Auto {
    Start(String),
    Stop,
    Nothing,
}

/// Auto mode: on when a listed game runs, off when the last one exits. An
/// auto boost the user turned off stays off until the games have exited
/// (`suppressed`). A manual boost is never turned off automatically.
fn auto_decision(auto: bool, active_reason: Option<&str>, running: &[String], suppressed: bool) -> Auto {
    if active_reason == Some("auto") && running.is_empty() {
        return Auto::Stop;
    }
    if !auto || active_reason.is_some() || suppressed {
        return Auto::Nothing;
    }
    match running.first() {
        Some(game) => Auto::Start(game.clone()),
        None => Auto::Nothing,
    }
}

// ---------------------------------------------------------------------------
// Files
// ---------------------------------------------------------------------------

fn dir_from(env: Option<std::ffi::OsString>, local_data: Option<PathBuf>) -> Option<PathBuf> {
    match env {
        Some(v) if !v.is_empty() => Some(PathBuf::from(v)),
        _ => local_data.map(|d| d.join("boost")),
    }
}

fn read_json<T: DeserializeOwned>(path: &Path) -> Option<T> {
    let bytes = fs::read(path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Write to a temporary file, then rename over the target, so a crash
/// mid-write never leaves half a file.
fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    let dir = path.parent().ok_or("no folder")?;
    fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let bytes = serde_json::to_vec_pretty(value).map_err(|e| e.to_string())?;
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, bytes).map_err(|e| e.to_string())?;
    fs::rename(&tmp, path).map_err(|e| e.to_string())
}

fn save_state(dir: Option<&Path>, saved: &Saved) -> Result<(), String> {
    let dir = dir.ok_or("no app data folder")?;
    write_json(&dir.join(STATE_FILE), saved)
}

fn clear_state(dir: Option<&Path>) {
    if let Some(dir) = dir {
        let _ = fs::remove_file(dir.join(STATE_FILE));
    }
}

/// Crash recovery: restores a boost left in `dir` and removes the file
/// (also when some processes could not be restored, so a start never loops).
fn recover_dir(dir: &Path, now: u64, ops: &dyn ProcOps, power: &dyn PowerOps) -> Option<RestoreReport> {
    let path = dir.join(STATE_FILE);
    if !path.exists() {
        return None;
    }
    let report = read_json::<Saved>(&path).map(|saved| restore_boost(&saved, now, ops, power));
    let _ = fs::remove_file(&path);
    report
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Real OS calls
// ---------------------------------------------------------------------------

struct RealProcs<'a> {
    /// Orbit's suspended list, so the Processes view shows paused apps and
    /// `actions::resume_all` wakes them if Orbit exits another way.
    suspended: Option<&'a SuspendedSet>,
}

fn open_verified(
    pid: u32,
    access: windows::Win32::System::Threading::PROCESS_ACCESS_RIGHTS,
    start: u64,
) -> Result<actions::OwnedHandle, String> {
    let handle = actions::open_process(pid, access | PROCESS_QUERY_LIMITED_INFORMATION)?;
    actions::verify_identity(handle.0, Some(start))?;
    // An exited process whose handle someone still holds keeps its pid and
    // start time; it counts as gone.
    if actions::handle_has_exited(handle.0) == Some(true) {
        return Err(EXITED.to_string());
    }
    Ok(handle)
}

impl ProcOps for RealProcs<'_> {
    fn check(&self, pid: u32, start: u64) -> Result<(), String> {
        actions::guard_pid(pid, "changed")?;
        let handle = match open_verified(pid, PROCESS_QUERY_LIMITED_INFORMATION, start) {
            Ok(h) => h,
            Err(e) => {
                actions::refuse_critical_suspend(pid, None)?;
                return Err(e);
            }
        };
        actions::refuse_critical_suspend(pid, Some(handle.0))
    }

    fn priority(&self, pid: u32, start: u64) -> Result<u32, String> {
        let handle = open_verified(pid, PROCESS_QUERY_LIMITED_INFORMATION, start)?;
        match unsafe { GetPriorityClass(handle.0) } {
            0 => Err(actions::describe(&windows::core::Error::from_win32(), "read the priority")),
            class => Ok(class),
        }
    }

    fn set_priority(&self, pid: u32, start: u64, class: u32) -> Result<(), String> {
        actions::guard_pid(pid, "changed")?;
        let handle = open_verified(pid, PROCESS_SET_INFORMATION, start)?;
        unsafe { SetPriorityClass(handle.0, PROCESS_CREATION_FLAGS(class)) }
            .map_err(|e| actions::describe(&e, "change the priority"))
    }

    fn suspend(&self, pid: u32, start: u64) -> Result<(), String> {
        actions::guard_pid(pid, "suspended")?;
        let handle = open_verified(pid, PROCESS_SUSPEND_RESUME, start)?;
        actions::refuse_critical_suspend(pid, Some(handle.0))?;
        let status = unsafe { actions::NtSuspendProcess(handle.0) };
        if status < 0 {
            return Err(actions::describe_ntstatus(status, "pause that process"));
        }
        if let Some(set) = self.suspended {
            actions::lock_suspended(set).insert(pid, start);
        }
        Ok(())
    }

    fn resume(&self, pid: u32, start: u64) -> Result<(), String> {
        let handle = open_verified(pid, PROCESS_SUSPEND_RESUME, start)?;
        let status = unsafe { actions::NtResumeProcess(handle.0) };
        if status < 0 {
            return Err(actions::describe_ntstatus(status, "resume that process"));
        }
        if let Some(set) = self.suspended {
            let mut set = actions::lock_suspended(set);
            if set.get(&pid).is_some_and(|&s| crate::model::same_start(s, start)) {
                set.remove(&pid);
            }
        }
        Ok(())
    }
}

struct RealPower;

fn win32_err(code: u32) -> String {
    windows::core::Error::from(HRESULT::from_win32(code)).message().trim().trim_end_matches('.').to_string()
}

impl PowerOps for RealPower {
    fn active(&self) -> Result<u128, String> {
        let mut ptr: *mut GUID = std::ptr::null_mut();
        let err = unsafe { PowerGetActiveScheme(HKEY::default(), &mut ptr) };
        if err.0 != 0 || ptr.is_null() {
            return Err(win32_err(err.0));
        }
        // SAFETY: Windows allocated a GUID for us; read it, then free it.
        let guid = unsafe { *ptr };
        unsafe {
            let _ = LocalFree(HLOCAL(ptr as *mut c_void));
        }
        Ok(guid.to_u128())
    }

    fn schemes(&self) -> Vec<(u128, String)> {
        let mut out = Vec::new();
        for index in 0..64u32 {
            let mut guid = GUID::zeroed();
            let mut size = std::mem::size_of::<GUID>() as u32;
            let err = unsafe {
                PowerEnumerate(
                    HKEY::default(),
                    None,
                    None,
                    ACCESS_SCHEME,
                    index,
                    Some(&mut guid as *mut GUID as *mut u8),
                    &mut size,
                )
            };
            if err.0 != 0 {
                break; // ERROR_NO_MORE_ITEMS, or anything else
            }
            out.push((guid.to_u128(), friendly_name(&guid)));
        }
        out
    }

    fn set_active(&self, guid: u128) -> Result<(), String> {
        let g = GUID::from_u128(guid);
        let err = unsafe { PowerSetActiveScheme(HKEY::default(), Some(&g as *const GUID)) };
        if err.0 != 0 {
            return Err(win32_err(err.0));
        }
        Ok(())
    }
}

fn friendly_name(guid: &GUID) -> String {
    let mut size = 0u32;
    unsafe {
        let _ = PowerReadFriendlyName(HKEY::default(), Some(guid as *const GUID), None, None, None, &mut size);
    }
    if size < 2 {
        return String::new();
    }
    let mut buf = vec![0u16; (size as usize).div_ceil(2)];
    let err = unsafe {
        PowerReadFriendlyName(HKEY::default(), Some(guid as *const GUID), None, None, Some(buf.as_mut_ptr() as *mut u8), &mut size)
    };
    if err.0 != 0 {
        return String::new();
    }
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

// ---------------------------------------------------------------------------
// Engine (one per process)
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Engine {
    initialized: bool,
    dir: Option<PathBuf>,
    config: BoostConfig,
    active: Option<Saved>,
    skipped: Vec<ProcNote>,
    /// (pid, start) already considered during this boost, so new processes
    /// of listed apps are picked up once.
    seen: HashSet<(u32, u64)>,
    power_note: Option<String>,
    suppressed: bool,
    running_games: Vec<String>,
    last_restore: Option<RestoreReport>,
    recovered: Option<RestoreReport>,
}

fn engine() -> MutexGuard<'static, Engine> {
    static ENGINE: OnceLock<Mutex<Engine>> = OnceLock::new();
    ENGINE
        .get_or_init(|| Mutex::new(Engine::default()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

fn with_procs<R>(app: &AppHandle, f: impl FnOnce(&RealProcs) -> R) -> R {
    let set = app.try_state::<SuspendedSet>();
    let procs = RealProcs { suspended: set.as_ref().map(|s| s.inner()) };
    f(&procs)
}

fn start_locked(app: &AppHandle, e: &mut Engine, rows: &[ProcessRow], reason: &str, trigger: Option<String>) -> Result<(), String> {
    let dir = e.dir.clone();
    let cfg = e.config.clone();
    let started = with_procs(app, |procs| {
        start_boost(&cfg, rows, &Context::current(), reason, trigger, now_secs(), procs, &RealPower, &mut |s| {
            save_state(dir.as_deref(), s)
        })
    })?;
    e.seen = started.saved.procs.iter().map(|t| (t.pid, t.start)).collect();
    for r in rows.iter().filter(|r| name_in(&cfg.apps, &r.name)) {
        e.seen.insert((r.pid, r.start_time));
    }
    e.active = Some(started.saved);
    e.skipped = started.skipped;
    e.power_note = started.power_note;
    ACTIVE.store(true, Ordering::Relaxed);
    Ok(())
}

/// Applies Boost to processes of listed apps that started since it turned on.
fn extend_locked(app: &AppHandle, e: &mut Engine, rows: &[ProcessRow]) -> bool {
    let Some(mut active) = e.active.clone() else {
        return false;
    };
    let cx = Context::current();
    let fresh: Vec<&ProcessRow> = pick_targets(rows, &e.config, &cx)
        .into_iter()
        .filter(|r| !e.seen.contains(&(r.pid, r.start_time)))
        .collect();
    if fresh.is_empty() {
        return false;
    }
    e.seen.extend(fresh.iter().map(|r| (r.pid, r.start_time)));
    // A boost keeps the mode it started with.
    let pause = active.procs.first().map_or(e.config.pause, |t| t.paused);
    let dir = e.dir.clone();
    let mut skipped = Vec::new();
    let applied = with_procs(app, |procs| {
        let planned = plan_procs(&fresh, pause, procs, &mut skipped);
        if planned.is_empty() {
            return Vec::new();
        }
        let mut intent = active.clone();
        intent.procs.extend(planned.iter().cloned());
        if save_state(dir.as_deref(), &intent).is_err() {
            return Vec::new();
        }
        apply_procs(planned, procs, &mut skipped)
    });
    let changed = !applied.is_empty() || !skipped.is_empty();
    e.skipped.extend(skipped);
    if !applied.is_empty() {
        active.procs.extend(applied);
        let _ = save_state(dir.as_deref(), &active);
        e.active = Some(active);
    }
    changed
}

fn stop_locked(app: &AppHandle, e: &mut Engine) {
    let Some(saved) = e.active.take() else {
        return;
    };
    let report = with_procs(app, |procs| restore_boost(&saved, now_secs(), procs, &RealPower));
    clear_state(e.dir.as_deref());
    e.last_restore = Some(report);
    e.skipped.clear();
    e.seen.clear();
    e.power_note = None;
    ACTIVE.store(false, Ordering::Relaxed);
}

fn plan_of(guid: u128, schemes: &[(u128, String)]) -> Plan {
    let name = schemes
        .iter()
        .find(|s| s.0 == guid)
        .map(|s| s.1.clone())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| "Unknown plan".to_string());
    Plan { guid: guid_string(guid), name }
}

fn status_of(e: &Engine, power: &dyn PowerOps) -> BoostStatus {
    let schemes = power.schemes();
    let mut p = PowerStatus {
        current: power.active().ok().map(|g| plan_of(g, &schemes)),
        target: choose_target(&schemes).map(|t| plan_of(t.0, &schemes)),
        ..Default::default()
    };
    if p.target.is_none() {
        p.note = Some(NO_PLAN.to_string());
    }
    let a = e.active.as_ref();
    if let Some(before) = a.and_then(|a| a.power_before.as_deref()).and_then(parse_guid) {
        p.before = Some(plan_of(before, &schemes));
    }
    if a.is_some() && e.power_note.is_some() {
        p.note = e.power_note.clone();
    }
    BoostStatus {
        active: a.is_some(),
        reason: a.map(|a| a.reason.clone()),
        trigger: a.and_then(|a| a.trigger.clone()),
        since: a.map(|a| a.started_at),
        config: e.config.clone(),
        power: p,
        changed: a
            .map(|a| {
                a.procs
                    .iter()
                    .map(|t| Changed {
                        pid: t.pid,
                        name: t.name.clone(),
                        action: if t.paused { "paused" } else { "lowered" }.to_string(),
                        before: actions::priority_name(t.before).map(str::to_string),
                    })
                    .collect()
            })
            .unwrap_or_default(),
        skipped: e.skipped.clone(),
        running_games: e.running_games.clone(),
        last_restore: e.last_restore.clone(),
        recovered: e.recovered.clone(),
    }
}

fn status(_app: &AppHandle) -> BoostStatus {
    status_of(&engine(), &RealPower)
}

fn emit_status(app: &AppHandle) {
    let _ = app.emit(BOOST_EVENT, status(app));
}

// ---------------------------------------------------------------------------
// Hooks called by lib.rs and the sampler
// ---------------------------------------------------------------------------

/// Setup hook, before the sampler starts: loads the config and restores a
/// boost a crash left on.
pub fn init(app: &AppHandle) {
    let dir = dir_from(std::env::var_os("ORBIT_BOOST_DIR"), app.path().app_local_data_dir().ok());
    let config = dir
        .as_ref()
        .and_then(|d| read_json::<BoostConfig>(&d.join(CONFIG_FILE)))
        .unwrap_or_default()
        .sanitized();
    let recovered = dir
        .as_deref()
        .and_then(|d| recover_dir(d, now_secs(), &RealProcs { suspended: None }, &RealPower));
    let mut e = engine();
    e.dir = dir;
    e.config = config;
    e.last_restore = recovered.clone();
    e.recovered = recovered;
    e.initialized = true;
}

/// Called by the sampler after every snapshot: auto mode, and processes of
/// listed apps that start while Boost is on.
pub fn on_snapshot(app: &AppHandle, snapshot: &Snapshot) {
    let changed = {
        let mut e = engine();
        if !e.initialized || (e.config.games.is_empty() && e.active.is_none()) {
            false
        } else {
            let rows = &snapshot.processes;
            let running = running_games(rows, &e.config.games);
            let mut changed = running != e.running_games;
            if running.is_empty() {
                e.suppressed = false;
            }
            let reason = e.active.as_ref().map(|a| a.reason.clone());
            match auto_decision(e.config.auto, reason.as_deref(), &running, e.suppressed) {
                Auto::Start(game) => {
                    if let Err(err) = start_locked(app, &mut e, rows, "auto", Some(game)) {
                        eprintln!("[orbit] auto boost failed: {err}");
                        e.suppressed = true; // don't retry every tick
                    }
                    changed = true;
                }
                Auto::Stop => {
                    stop_locked(app, &mut e);
                    changed = true;
                }
                Auto::Nothing => changed |= extend_locked(app, &mut e, rows),
            }
            e.running_games = running;
            changed
        }
    };
    if changed {
        emit_status(app);
    }
}

/// `crate::exit_cleanup`: puts everything back before Orbit goes. Only this
/// process's own boost (the state file of a crashed one is `init`'s job).
pub fn on_exit(app: &AppHandle) {
    let mut e = engine();
    if e.active.is_some() {
        stop_locked(app, &mut e);
    }
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(f).await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn boost_status(app: AppHandle) -> Result<BoostStatus, String> {
    blocking(move || status(&app)).await
}

/// Stores the UI's lists and toggles (applied from the next boost on).
#[tauri::command]
pub async fn boost_configure(app: AppHandle, config: BoostConfig) -> Result<BoostStatus, String> {
    blocking(move || {
        {
            let mut e = engine();
            e.config = config.sanitized();
            if let Some(dir) = e.dir.as_deref() {
                if let Err(err) = write_json(&dir.join(CONFIG_FILE), &e.config) {
                    eprintln!("[orbit] could not save the boost settings: {err}");
                }
            }
            let rows = app.state::<AppState>().latest_snapshot().map(|s| s.processes).unwrap_or_default();
            e.running_games = running_games(&rows, &e.config.games);
        }
        emit_status(&app);
        status(&app)
    })
    .await
}

#[tauri::command]
pub async fn boost_start(app: AppHandle) -> Result<BoostStatus, String> {
    blocking(move || {
        let rows = app
            .state::<AppState>()
            .latest_snapshot()
            .map(|s| s.processes)
            .ok_or("Orbit has not read the process list yet. Try again in a moment.")?;
        {
            let mut e = engine();
            if e.active.is_none() {
                start_locked(&app, &mut e, &rows, "manual", None)?;
            }
        }
        emit_status(&app);
        Ok(status(&app))
    })
    .await?
}

#[tauri::command]
pub async fn boost_stop(app: AppHandle) -> Result<BoostStatus, String> {
    blocking(move || {
        {
            let mut e = engine();
            if e.active.as_ref().is_some_and(|a| a.reason == "auto") && !e.running_games.is_empty() {
                e.suppressed = true;
            }
            stop_locked(&app, &mut e);
        }
        emit_status(&app);
        status(&app)
    })
    .await
}

#[tauri::command]
pub async fn boost_suggestions(app: AppHandle) -> Result<Vec<Suggestion>, String> {
    blocking(move || {
        let rows = app.state::<AppState>().latest_snapshot().map(|s| s.processes).unwrap_or_default();
        let cfg = engine().config.clone();
        suggestions(&rows, &cfg, &Context::current())
    })
    .await
}

/// A file picker for a game's exe. Ok(None) when cancelled.
#[tauri::command]
pub async fn boost_pick_exe(app: AppHandle) -> Result<Option<String>, String> {
    let owner = app
        .get_webview_window("main")
        .and_then(|w| w.hwnd().ok())
        .map(|h| h.0 as isize)
        .unwrap_or(0);
    blocking(move || {
        std::thread::spawn(move || pick_exe(owner))
            .join()
            .unwrap_or_else(|_| Err("The file picker failed.".to_string()))
    })
    .await?
}

fn pick_exe(owner: isize) -> Result<Option<String>, String> {
    struct Com;
    impl Drop for Com {
        fn drop(&mut self) {
            unsafe { CoUninitialize() };
        }
    }
    let fail = |e: windows::core::Error| format!("Could not open the file picker: {}", e.message());
    unsafe {
        let _com = CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE).is_ok().then_some(Com);
        let dialog: IFileOpenDialog = CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).map_err(fail)?;
        let filters = [COMDLG_FILTERSPEC { pszName: w!("Programs (*.exe)"), pszSpec: w!("*.exe") }];
        dialog.SetFileTypes(&filters).map_err(fail)?;
        dialog.SetTitle(w!("Pick a game")).map_err(fail)?;
        let options = dialog.GetOptions().map_err(fail)?;
        dialog
            .SetOptions(options | FOS_FILEMUSTEXIST | FOS_FORCEFILESYSTEM | FOS_NOCHANGEDIR)
            .map_err(fail)?;
        if let Err(e) = dialog.Show(HWND(owner as *mut c_void)) {
            if e.code() == HRESULT::from_win32(ERROR_CANCELLED.0) {
                return Ok(None);
            }
            return Err(fail(e));
        }
        let item = dialog.GetResult().map_err(fail)?;
        let path = item.GetDisplayName(SIGDN_FILESYSPATH).map_err(fail)?;
        let text = path.to_string().unwrap_or_default();
        CoTaskMemFree(Some(path.0 as *const c_void));
        Ok(Some(text).filter(|t| !t.is_empty()))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashMap;
    use windows::Win32::System::Threading::IDLE_PRIORITY_CLASS;

    const NORMAL: u32 = NORMAL_PRIORITY_CLASS.0;
    const ABOVE: u32 = ABOVE_NORMAL_PRIORITY_CLASS.0;
    const HIGH: u32 = HIGH_PRIORITY_CLASS.0;
    const IDLE: u32 = IDLE_PRIORITY_CLASS.0;
    const BALANCED: u128 = 0x381b4222_f694_41f0_9685_ff5bb260df2e;
    const ME: u32 = 999;

    fn cx() -> Context {
        Context { own_pid: ME, own_session: Some(1) }
    }

    fn row(pid: u32, name: &str, kind: ProcessKind, session: Option<u32>) -> ProcessRow {
        ProcessRow {
            pid,
            parent_pid: None,
            name: name.to_string(),
            exe: Some(format!(r"C:\Apps\{name}")),
            cmd: String::new(),
            user: None,
            cpu: 0.0,
            memory: 0,
            disk_read_bps: 0.0,
            disk_write_bps: 0.0,
            gpu: None,
            status: "running".into(),
            start_time: 1000 + pid as u64,
            kind,
            window_title: None,
            not_responding: false,
            suspended: false,
            session_id: session,
            net_rx_bps: None,
            net_tx_bps: None,
        }
    }

    fn bg(pid: u32, name: &str) -> ProcessRow {
        row(pid, name, ProcessKind::Background, Some(1))
    }

    #[derive(Clone)]
    struct FakeProc {
        start: u64,
        class: u32,
        suspends: i32,
        alive: bool,
        refuse: Option<&'static str>,
    }

    #[derive(Default)]
    struct FakeProcs {
        procs: RefCell<HashMap<u32, FakeProc>>,
        log: RefCell<Vec<String>>,
        fail_set: RefCell<HashSet<u32>>,
    }

    impl FakeProcs {
        fn with(list: &[(u32, u32)]) -> Self {
            let f = FakeProcs::default();
            for &(pid, class) in list {
                f.procs.borrow_mut().insert(
                    pid,
                    FakeProc { start: 1000 + pid as u64, class, suspends: 0, alive: true, refuse: None },
                );
            }
            f
        }
        fn get(&self, pid: u32, start: u64) -> Result<FakeProc, String> {
            match self.procs.borrow().get(&pid) {
                Some(p) if p.alive && p.start == start => Ok(p.clone()),
                _ => Err(EXITED.to_string()),
            }
        }
        fn class(&self, pid: u32) -> u32 {
            self.procs.borrow()[&pid].class
        }
        fn suspends(&self, pid: u32) -> i32 {
            self.procs.borrow()[&pid].suspends
        }
        fn edit(&self, pid: u32, f: impl FnOnce(&mut FakeProc)) {
            f(self.procs.borrow_mut().get_mut(&pid).unwrap());
        }
    }

    impl ProcOps for FakeProcs {
        fn check(&self, pid: u32, start: u64) -> Result<(), String> {
            match self.get(pid, start)?.refuse {
                Some(r) => Err(r.to_string()),
                None => Ok(()),
            }
        }
        fn priority(&self, pid: u32, start: u64) -> Result<u32, String> {
            Ok(self.get(pid, start)?.class)
        }
        fn set_priority(&self, pid: u32, start: u64, class: u32) -> Result<(), String> {
            self.get(pid, start)?;
            if self.fail_set.borrow().contains(&pid) {
                return Err("Access denied. Try Restart as administrator.".into());
            }
            self.log.borrow_mut().push(format!("set {pid} {class:#x}"));
            self.edit(pid, |p| p.class = class);
            Ok(())
        }
        fn suspend(&self, pid: u32, start: u64) -> Result<(), String> {
            self.get(pid, start)?;
            self.log.borrow_mut().push(format!("suspend {pid}"));
            self.edit(pid, |p| p.suspends += 1);
            Ok(())
        }
        fn resume(&self, pid: u32, start: u64) -> Result<(), String> {
            self.get(pid, start)?;
            self.log.borrow_mut().push(format!("resume {pid}"));
            self.edit(pid, |p| p.suspends = (p.suspends - 1).max(0));
            Ok(())
        }
    }

    struct FakePower {
        active: RefCell<u128>,
        schemes: Vec<(u128, String)>,
        fail_set: bool,
        log: RefCell<Vec<String>>,
    }

    impl FakePower {
        fn new(active: u128, schemes: &[(u128, &str)]) -> Self {
            FakePower {
                active: RefCell::new(active),
                schemes: schemes.iter().map(|(g, n)| (*g, n.to_string())).collect(),
                fail_set: false,
                log: RefCell::new(Vec::new()),
            }
        }
        fn standard() -> Self {
            Self::new(BALANCED, &[(BALANCED, "Balanced"), (HIGH_PERFORMANCE, "High performance")])
        }
        fn now(&self) -> u128 {
            *self.active.borrow()
        }
    }

    impl PowerOps for FakePower {
        fn active(&self) -> Result<u128, String> {
            Ok(*self.active.borrow())
        }
        fn schemes(&self) -> Vec<(u128, String)> {
            self.schemes.clone()
        }
        fn set_active(&self, guid: u128) -> Result<(), String> {
            if self.fail_set {
                return Err("Access is denied".into());
            }
            self.log.borrow_mut().push(format!("power {}", guid_string(guid)));
            *self.active.borrow_mut() = guid;
            Ok(())
        }
    }

    fn cfg(apps: &[&str]) -> BoostConfig {
        BoostConfig { apps: apps.iter().map(|s| s.to_string()).collect(), ..Default::default() }
    }

    fn start(
        c: &BoostConfig,
        rows: &[ProcessRow],
        ops: &FakeProcs,
        power: &FakePower,
        saves: &RefCell<Vec<Saved>>,
    ) -> Result<Started, String> {
        start_boost(c, rows, &cx(), "manual", None, 50, ops, power, &mut |s| {
            ops.log.borrow_mut().push("save".into());
            saves.borrow_mut().push(s.clone());
            Ok(())
        })
    }

    #[test]
    fn picks_only_listed_user_apps() {
        let mut c = cfg(&["chrome.exe", "Svc.exe", "game.exe", "sys.exe", "orbit.exe", "other.exe"]);
        c.games = vec!["GAME.EXE".into()];
        let rows = vec![
            bg(10, "Chrome.exe"),
            bg(11, "chrome.exe"),
            row(12, "svc.exe", ProcessKind::Background, Some(0)),
            bg(13, "game.exe"),
            row(14, "sys.exe", ProcessKind::Windows, Some(1)),
            bg(15, "orbit.exe"),
            bg(ME, "notorbit.exe"),
            row(16, "other.exe", ProcessKind::App, Some(2)),
            row(17, "other.exe", ProcessKind::App, None),
            bg(18, "unlisted.exe"),
            ProcessRow { exe: Some(r"C:\WINDOWS\explorer.exe".into()), ..row(19, "chrome.exe", ProcessKind::App, Some(1)) },
        ];
        let picked: Vec<u32> = pick_targets(&rows, &c, &cx()).iter().map(|r| r.pid).collect();
        assert_eq!(picked, vec![10, 11]);
        let mut me_listed = cfg(&["notorbit.exe"]);
        me_listed.games.clear();
        assert!(pick_targets(&rows, &me_listed, &cx()).is_empty(), "Orbit's own pid");
    }

    #[test]
    fn lowers_and_restores_exactly() {
        let rows = vec![bg(10, "a.exe"), bg(11, "a.exe"), bg(12, "b.exe"), bg(13, "c.exe"), bg(14, "d.exe")];
        let ops = FakeProcs::with(&[(10, NORMAL), (11, ABOVE), (12, IDLE), (13, HIGH), (14, BELOW)]);
        let power = FakePower::standard();
        let saves = RefCell::new(Vec::new());
        let c = BoostConfig { power: false, ..cfg(&["a.exe", "b.exe", "c.exe", "d.exe"]) };
        let s = start(&c, &rows, &ops, &power, &saves).unwrap();
        let pids: Vec<u32> = s.saved.procs.iter().map(|t| t.pid).collect();
        assert_eq!(pids, vec![10, 11, 13]);
        for pid in [10, 11, 13] {
            assert_eq!(ops.class(pid), BELOW);
        }
        assert_eq!(ops.class(12), IDLE, "idle is never raised");
        assert_eq!(s.skipped.iter().map(|n| n.pid).collect::<Vec<_>>(), vec![12, 14]);
        // The plan is on disk before the first change.
        assert_eq!(ops.log.borrow()[0], "save");
        assert_eq!(saves.borrow()[0].procs.len(), 3);
        assert_eq!(power.now(), BALANCED, "power toggle off");

        let r = restore_boost(&s.saved, 60, &ops, &power);
        assert_eq!((r.restored, r.exited, r.left_alone), (3, 0, 0));
        assert!(r.failed.is_empty());
        assert_eq!(r.power, "none");
        assert_eq!((ops.class(10), ops.class(11), ops.class(12), ops.class(13), ops.class(14)), (NORMAL, ABOVE, IDLE, HIGH, BELOW));
    }

    #[test]
    fn exited_reused_and_changed_processes_are_skipped_quietly() {
        let rows = vec![bg(10, "a.exe"), bg(11, "a.exe"), bg(12, "a.exe"), bg(13, "a.exe")];
        let ops = FakeProcs::with(&[(10, NORMAL), (11, NORMAL), (12, NORMAL), (13, NORMAL)]);
        let power = FakePower::standard();
        let saves = RefCell::new(Vec::new());
        let s = start(&cfg(&["a.exe"]), &rows, &ops, &power, &saves).unwrap();
        assert_eq!(s.saved.procs.len(), 4);
        ops.edit(10, |p| p.alive = false); // exited
        ops.edit(11, |p| p.start += 50); // pid reused by a new process
        ops.edit(12, |p| p.class = HIGH); // the user changed it during Boost
        let r = restore_boost(&s.saved, 60, &ops, &power);
        assert_eq!((r.restored, r.exited, r.left_alone), (1, 2, 1));
        assert!(r.failed.is_empty());
        assert_eq!(ops.class(11), BELOW, "the new process with the reused pid is not touched");
        assert_eq!(ops.class(12), HIGH);
        assert_eq!(ops.class(13), NORMAL);
    }

    #[test]
    fn pause_mode_suspends_and_resumes() {
        let mut paused_row = bg(12, "a.exe");
        paused_row.suspended = true;
        let rows = vec![bg(10, "a.exe"), bg(11, "a.exe"), paused_row];
        let ops = FakeProcs::with(&[(10, NORMAL), (11, IDLE), (12, NORMAL)]);
        let power = FakePower::standard();
        let saves = RefCell::new(Vec::new());
        let c = BoostConfig { pause: true, power: false, ..cfg(&["a.exe"]) };
        let s = start(&c, &rows, &ops, &power, &saves).unwrap();
        assert_eq!(s.saved.procs.iter().map(|t| (t.pid, t.paused)).collect::<Vec<_>>(), vec![(10, true), (11, true)]);
        assert_eq!((ops.suspends(10), ops.suspends(11), ops.suspends(12)), (1, 1, 0));
        assert_eq!(ops.class(10), NORMAL, "pausing leaves priority alone");
        assert_eq!(s.skipped[0].reason, "Already paused in Orbit.");
        let r = restore_boost(&s.saved, 60, &ops, &power);
        assert_eq!(r.restored, 2);
        assert_eq!((ops.suspends(10), ops.suspends(11)), (0, 0));
    }

    #[test]
    fn refusals_and_failures_are_reported() {
        let rows = vec![bg(10, "a.exe"), bg(11, "a.exe"), bg(12, "a.exe")];
        let ops = FakeProcs::with(&[(10, NORMAL), (11, NORMAL), (12, NORMAL)]);
        ops.edit(10, |p| p.refuse = Some("That process is part of Orbit."));
        ops.fail_set.borrow_mut().insert(11);
        let power = FakePower::standard();
        let saves = RefCell::new(Vec::new());
        let s = start(&BoostConfig { power: false, ..cfg(&["a.exe"]) }, &rows, &ops, &power, &saves).unwrap();
        assert_eq!(s.saved.procs.iter().map(|t| t.pid).collect::<Vec<_>>(), vec![12]);
        let reasons: Vec<_> = s.skipped.iter().map(|n| (n.pid, n.reason.as_str())).collect();
        assert_eq!(reasons, vec![(10, "That process is part of Orbit."), (11, "Access denied. Try Restart as administrator.")]);
        // The final save lists only what took effect.
        assert_eq!(saves.borrow().last().unwrap().procs.len(), 1);
    }

    #[test]
    fn nothing_changes_when_the_plan_cannot_be_saved() {
        let rows = vec![bg(10, "a.exe")];
        let ops = FakeProcs::with(&[(10, NORMAL)]);
        let power = FakePower::standard();
        let err = start_boost(&cfg(&["a.exe"]), &rows, &cx(), "manual", None, 1, &ops, &power, &mut |_| {
            Err("disk full".into())
        });
        assert!(err.is_err_and(|e| e.contains("disk full")));
        assert_eq!(ops.class(10), NORMAL);
        assert_eq!(power.now(), BALANCED);
        assert!(ops.log.borrow().is_empty());
    }

    #[test]
    fn switches_and_restores_the_power_plan() {
        let power = FakePower::new(
            BALANCED,
            &[(BALANCED, "Balanced"), (HIGH_PERFORMANCE, "High performance"), (0x1234, "Ultimate Performance")],
        );
        let ops = FakeProcs::default();
        let saves = RefCell::new(Vec::new());
        let s = start(&cfg(&[]), &[], &ops, &power, &saves).unwrap();
        assert_eq!(power.now(), 0x1234, "Ultimate (a copy, matched by name) beats High");
        assert_eq!(saves.borrow()[0].power_before.as_deref(), Some("381b4222-f694-41f0-9685-ff5bb260df2e"));
        assert_eq!(power.log.borrow().len(), 1, "one switch");
        let r = restore_boost(&s.saved, 2, &ops, &power);
        assert_eq!(r.power, "restored");
        assert_eq!(power.now(), BALANCED);

        // The user picked another plan during Boost: it stays.
        let s = start(&cfg(&[]), &[], &ops, &power, &saves).unwrap();
        *power.active.borrow_mut() = HIGH_PERFORMANCE;
        let r = restore_boost(&s.saved, 3, &ops, &power);
        assert_eq!(r.power, "left");
        assert_eq!(power.now(), HIGH_PERFORMANCE);
    }

    #[test]
    fn power_notes() {
        let ops = FakeProcs::default();
        let saves = RefCell::new(Vec::new());
        // Already on High performance: nothing to switch or restore.
        let on_high = FakePower::new(HIGH_PERFORMANCE, &[(BALANCED, "Balanced"), (HIGH_PERFORMANCE, "Hohe Leistung")]);
        let s = start(&cfg(&[]), &[], &ops, &on_high, &saves).unwrap();
        assert!(s.saved.power_before.is_none() && s.power_note.is_none());
        assert_eq!(restore_boost(&s.saved, 1, &ops, &on_high).power, "none");
        // No target plan at all.
        let only_balanced = FakePower::new(BALANCED, &[(BALANCED, "Balanced")]);
        let s = start(&cfg(&[]), &[], &ops, &only_balanced, &saves).unwrap();
        assert_eq!(s.power_note.as_deref(), Some(NO_PLAN));
        assert_eq!(only_balanced.now(), BALANCED);
        // Switching refused: Boost still runs, nothing to restore.
        let mut refused = FakePower::standard();
        refused.fail_set = true;
        let s = start(&cfg(&[]), &[], &ops, &refused, &saves).unwrap();
        assert!(s.power_note.unwrap().starts_with("Could not switch"));
        assert!(s.saved.power_before.is_none());
        assert_eq!(saves.borrow().last().unwrap().power_set, None);
    }

    #[test]
    fn chooses_the_target_plan() {
        let s = |list: &[(u128, &str)]| list.iter().map(|(g, n)| (*g, n.to_string())).collect::<Vec<_>>();
        assert_eq!(choose_target(&s(&[(BALANCED, "Balanced")])), None);
        assert_eq!(choose_target(&s(&[(BALANCED, "Balanced"), (HIGH_PERFORMANCE, "x")])).unwrap().0, HIGH_PERFORMANCE);
        assert_eq!(choose_target(&s(&[(HIGH_PERFORMANCE, "x"), (ULTIMATE, "y")])).unwrap().0, ULTIMATE);
        assert_eq!(choose_target(&s(&[(7, "high performance ")])).unwrap().0, 7);
    }

    #[test]
    fn formats_and_parses_guids() {
        assert_eq!(guid_string(HIGH_PERFORMANCE), "8c5e7fda-e8bf-4a96-9a85-a6e23a8c635c");
        assert_eq!(parse_guid("{8C5E7FDA-E8BF-4A96-9A85-A6E23A8C635C}"), Some(HIGH_PERFORMANCE));
        assert_eq!(parse_guid("nope"), None);
        // Same layout as the windows crate's GUID.
        let g = GUID::from_u128(HIGH_PERFORMANCE);
        assert_eq!((g.data1, g.data2, g.data3), (0x8c5e7fda, 0xe8bf, 0x4a96));
    }

    #[test]
    fn auto_mode_decisions() {
        let none: Vec<String> = vec![];
        let game = vec!["Game.exe".to_string()];
        assert_eq!(auto_decision(true, None, &game, false), Auto::Start("Game.exe".into()));
        assert_eq!(auto_decision(false, None, &game, false), Auto::Nothing);
        assert_eq!(auto_decision(true, None, &game, true), Auto::Nothing, "turned off by hand");
        assert_eq!(auto_decision(true, Some("auto"), &game, false), Auto::Nothing);
        assert_eq!(auto_decision(true, Some("auto"), &none, false), Auto::Stop);
        assert_eq!(auto_decision(false, Some("auto"), &none, false), Auto::Stop, "auto turned off mid-game");
        assert_eq!(auto_decision(true, Some("manual"), &none, false), Auto::Nothing);
        assert_eq!(auto_decision(true, None, &none, false), Auto::Nothing);
    }

    #[test]
    fn running_games_are_named_once() {
        let rows = vec![bg(1, "Game.exe"), bg(2, "game.exe"), bg(3, "b.exe"), bg(4, "Other.exe")];
        let games = vec!["GAME.exe".to_string(), "other.EXE".to_string()];
        assert_eq!(running_games(&rows, &games), vec!["Game.exe", "Other.exe"]);
        assert!(running_games(&rows, &[]).is_empty());
    }

    #[test]
    fn sanitizes_config() {
        let raw = BoostConfig {
            games: vec![r#""C:\Games\Game.exe""#.into(), "game.EXE".into(), "  ".into(), "D:/x/Two.exe".into()],
            apps: vec!["bad\u{7}.exe".into(), "Chrome.exe".into()],
            ..Default::default()
        };
        let c = raw.sanitized();
        assert_eq!(c.games, vec!["Game.exe", "Two.exe"]);
        assert_eq!(c.apps, vec!["Chrome.exe"]);
        assert!(c.power && !c.pause && !c.auto);
        let parsed: BoostConfig = serde_json::from_str(r#"{"apps":["a.exe"]}"#).unwrap();
        assert!(parsed.power, "missing keys use the defaults");
        let many = BoostConfig { apps: (0..100).map(|i| format!("{i}.exe")).collect(), ..Default::default() };
        assert_eq!(many.sanitized().apps.len(), MAX_NAMES);
    }

    #[test]
    fn suggests_heavy_user_apps() {
        let mut rows = vec![
            bg(10, "chrome.exe"),
            bg(11, "chrome.exe"),
            row(12, "dwm.exe", ProcessKind::Windows, Some(1)),
            bg(13, "game.exe"),
            bg(14, "tiny.exe"),
            bg(15, "msedgewebview2.exe"),
            row(16, "Discord.exe", ProcessKind::App, Some(1)),
        ];
        rows[0].cpu = 3.0;
        rows[0].memory = 300 << 20;
        rows[1].cpu = 2.0;
        rows[2].cpu = 50.0;
        rows[3].cpu = 80.0;
        rows[4].cpu = 0.2;
        rows[5].cpu = 9.0;
        rows[6].memory = 400 << 20;
        let mut c = cfg(&[]);
        c.games = vec!["game.exe".into()];
        let s = suggestions(&rows, &c, &cx());
        let names: Vec<_> = s.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["chrome.exe", "Discord.exe"]);
        assert_eq!((s[0].count, s[0].cpu), (2, 5.0));
        assert!(s[1].has_window && !s[0].has_window);
    }

    fn scratch(name: &str) -> PathBuf {
        let base = std::env::var_os("CARGO_TARGET_DIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
        let dir = base.join("boost-tests").join(name);
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn crash_recovery_from_the_state_file() {
        let dir = scratch("recovery");
        let rows = vec![bg(10, "a.exe"), bg(11, "a.exe")];
        let ops = FakeProcs::with(&[(10, NORMAL), (11, ABOVE)]);
        let power = FakePower::standard();
        let s = start_boost(&cfg(&["a.exe"]), &rows, &cx(), "auto", Some("game.exe".into()), 5, &ops, &power, &mut |s| {
            save_state(Some(&dir), s)
        })
        .unwrap();
        assert!(dir.join(STATE_FILE).exists());
        let on_disk: Saved = read_json(&dir.join(STATE_FILE)).unwrap();
        assert_eq!(on_disk, s.saved);
        assert_eq!(power.now(), HIGH_PERFORMANCE);
        // "Orbit crashed": the next start finds the file and puts everything back.
        ops.edit(11, |p| p.alive = false);
        let r = recover_dir(&dir, 9, &ops, &power).unwrap();
        assert_eq!((r.restored, r.exited, r.power.as_str()), (1, 1, "restored"));
        assert_eq!(ops.class(10), NORMAL);
        assert_eq!(power.now(), BALANCED);
        assert!(!dir.join(STATE_FILE).exists());
        assert!(recover_dir(&dir, 10, &ops, &power).is_none());
        // A damaged file is removed without touching anything.
        fs::write(dir.join(STATE_FILE), b"{not json").unwrap();
        assert!(recover_dir(&dir, 11, &ops, &power).is_none());
        assert!(!dir.join(STATE_FILE).exists());
    }

    #[test]
    fn boost_dir_override() {
        let local = Some(PathBuf::from(r"C:\Local"));
        assert_eq!(dir_from(Some("D:\\t".into()), local.clone()), Some(PathBuf::from("D:\\t")));
        assert_eq!(dir_from(Some("".into()), local.clone()), Some(PathBuf::from(r"C:\Local\boost")));
        assert_eq!(dir_from(None, None), None);
    }

    #[test]
    fn real_power_reads_without_changing() {
        let power = RealPower;
        let active = power.active().expect("active plan");
        let schemes = power.schemes();
        assert!(schemes.iter().any(|s| s.0 == active), "the active plan is listed");
        assert!(schemes.iter().all(|s| !s.1.is_empty()), "every plan has a name");
        assert_eq!(power.active().unwrap(), active);
    }

    #[test]
    fn real_check_refuses_orbit_and_system() {
        let ops = RealProcs { suspended: None };
        assert!(ops.check(4, 0).is_err());
        assert!(ops.check(std::process::id(), 0).is_err());
        assert_eq!(ops.priority(0x7fff_fff0, 1), Err(EXITED.to_string()));
    }

    // ---------------------------------------------------------------- live

    fn powercfg_active() -> String {
        let out = std::process::Command::new("powercfg").arg("/getactivescheme").output().expect("powercfg");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn spawn_ping(count: u32) -> std::process::Child {
        use std::os::windows::process::CommandExt;
        std::process::Command::new("ping")
            .args(["-n", &count.to_string(), "127.0.0.1"])
            .stdout(std::process::Stdio::null())
            .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
            .spawn()
            .expect("spawn ping")
    }

    fn live_row(child: &std::process::Child) -> ProcessRow {
        let pid = child.id();
        let handle = actions::open_process(pid, PROCESS_QUERY_LIMITED_INFORMATION).unwrap();
        let start = actions::filetime_to_unix(actions::handle_creation_filetime(handle.0).unwrap());
        let mut session = 0;
        unsafe { ProcessIdToSessionId(pid, &mut session) }.unwrap();
        ProcessRow { start_time: start, ..row(pid, "PING.EXE", ProcessKind::Background, Some(session)) }
    }

    /// The real power API, but without Ultimate Performance in the list, so the
    /// live test switches even on a PC that already runs Ultimate.
    struct HighOnly;
    impl PowerOps for HighOnly {
        fn active(&self) -> Result<u128, String> {
            RealPower.active()
        }
        fn schemes(&self) -> Vec<(u128, String)> {
            RealPower.schemes().into_iter().filter(|s| s.0 == HIGH_PERFORMANCE || (s.0 != ULTIMATE && !s.1.eq_ignore_ascii_case("Ultimate Performance") && s.0 != HIGH_PERFORMANCE && !s.1.eq_ignore_ascii_case("High performance"))).collect()
        }
        fn set_active(&self, guid: u128) -> Result<(), String> {
            RealPower.set_active(guid)
        }
    }

    /// Puts the power plan back if the live test panics half way.
    struct PlanGuard(u128);
    impl Drop for PlanGuard {
        fn drop(&mut self) {
            if RealPower.active().ok() != Some(self.0) {
                let _ = RealPower.set_active(self.0);
            }
        }
    }

    /// Boosts only ping processes this test starts, with the real Windows
    /// calls, then restores through the crash recovery path and checks that
    /// everything is back. With `ORBIT_BOOST_LIVE_POWER=1` it also does one
    /// real power plan switch (to High performance) and restore.
    /// `cargo test boost::tests::live -- --ignored --nocapture --test-threads=1`
    #[test]
    #[ignore]
    fn live_boost_own_processes() {
        let cfg_before = powercfg_active();
        let plan_before = RealPower.active().unwrap();
        let _guard = PlanGuard(plan_before);
        println!("before: {cfg_before}");
        let ops = RealProcs { suspended: None };
        let cx = Context::current();
        let dir = scratch("live");
        // High performance if the PC has it and is not on it, else whatever choose_target picks.
        let power = HighOnly;

        // 1. Lower priority + power plan. One ping starts at Idle and must stay so.
        let mut kids = vec![spawn_ping(60), spawn_ping(60), spawn_ping(60)];
        let rows: Vec<ProcessRow> = kids.iter().map(live_row).collect();
        let idle = &rows[2];
        ops.set_priority(idle.pid, idle.start_time, IDLE).unwrap();
        // The real plan switch runs only when asked for (once per check-up).
        let live_power = std::env::var("ORBIT_BOOST_LIVE_POWER").is_ok_and(|v| v == "1");
        let c = BoostConfig { apps: vec!["ping.exe".into()], power: live_power, ..Default::default() };
        let s = start_boost(&c, &rows, &cx, "manual", None, now_secs(), &ops, &power, &mut |s| {
            save_state(Some(&dir), s)
        })
        .unwrap();
        println!("boosted: {:?} skipped: {:?} note: {:?}", s.saved.procs, s.skipped, s.power_note);
        assert_eq!(s.saved.procs.len(), 2);
        assert_eq!(ops.priority(rows[0].pid, rows[0].start_time).unwrap(), BELOW);
        assert_eq!(ops.priority(rows[1].pid, rows[1].start_time).unwrap(), BELOW);
        assert_eq!(ops.priority(idle.pid, idle.start_time).unwrap(), IDLE);
        let target = choose_target(&power.schemes()).map(|t| t.0);
        match target {
            Some(t) if live_power && t != plan_before => {
                assert_eq!(RealPower.active().unwrap(), t);
                println!("during: {}", powercfg_active());
            }
            _ => println!("no switch on this PC: {:?}", s.power_note),
        }
        // One boosted ping exits during Boost: skipped quietly on restore.
        kids[1].kill().unwrap();
        let _ = kids[1].wait();
        let r = recover_dir(&dir, now_secs(), &ops, &power).expect("state file");
        println!("restore: {r:?}");
        assert_eq!((r.restored, r.exited, r.left_alone), (1, 1, 0));
        assert!(r.failed.is_empty());
        assert_eq!(ops.priority(rows[0].pid, rows[0].start_time).unwrap(), NORMAL);
        assert_eq!(ops.priority(idle.pid, idle.start_time).unwrap(), IDLE);
        assert_eq!(RealPower.active().unwrap(), plan_before);
        let cfg_after = powercfg_active();
        println!("after: {cfg_after}");
        assert_eq!(cfg_after, cfg_before);
        for k in kids.iter_mut() {
            let _ = k.kill();
            let _ = k.wait();
        }

        // 2. Pause, no power plan. A 4-ping process takes about 3 s; paused
        // for 5 s it must still be running, and finish once resumed.
        let mut short = spawn_ping(4);
        let rows = vec![live_row(&short)];
        let c = BoostConfig { apps: vec!["PING.exe".into()], pause: true, power: false, ..Default::default() };
        let s = start_boost(&c, &rows, &cx, "manual", None, now_secs(), &ops, &RealPower, &mut |s| {
            save_state(Some(&dir), s)
        })
        .unwrap();
        assert_eq!(s.saved.procs.len(), 1);
        assert!(s.saved.procs[0].paused);
        std::thread::sleep(std::time::Duration::from_secs(5));
        assert!(short.try_wait().unwrap().is_none(), "paused ping finished anyway");
        let r = restore_boost(&s.saved, now_secs(), &ops, &RealPower);
        clear_state(Some(&dir));
        assert_eq!(r.restored, 1);
        assert_eq!(r.power, "none");
        let t = std::time::Instant::now();
        while short.try_wait().unwrap().is_none() {
            assert!(t.elapsed().as_secs() < 10, "resumed ping did not finish");
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        assert_eq!(RealPower.active().unwrap(), plan_before);
        assert_eq!(powercfg_active(), cfg_before);
    }
}
