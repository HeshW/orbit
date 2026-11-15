//! Per-app rules (v0.9), Optimize > Rules.
//!
//! A rule says "whenever this app runs, give it this CPU priority (and these
//! cores)". It matches by full exe path or by exe name and sets a priority
//! (Low to High, never Realtime), an affinity mask, or efficiency mode
//! (Windows 11). A rule can apply only while the app's window is in front, or
//! only while Boost is on.
//!
//! No watcher or extra query of its own for new processes: the sampler calls
//! `on_snapshot` after every tick, and a process that appears there (a new pid
//! and start time, like Boost's detection) is checked against the rules and
//! changed within that same tick. When the sampler is paused or slower than a
//! second, it calls `light_tick` about once a second instead, which lists
//! processes with one Toolhelp snapshot and opens only the new ones. Processes
//! that have a rule are then re-checked each tick with one cheap read
//! (priority class, affinity mask, power throttling). If something changed a
//! value Orbit set, Orbit sets it again; if that happens 3 times within 60
//! seconds it stops for that process and says so, instead of fighting in a
//! loop. Orbit's own changes (the window moving to the back, Boost ending)
//! never count.
//!
//! "Only while in front" listens to `EVENT_SYSTEM_FOREGROUND` with
//! `SetWinEventHook` on one thread that sleeps in `GetMessageW` (no polling).
//!
//! Safety:
//! - Every change goes through `actions::set_priority` / `actions::set_affinity`
//!   (so `guard_pid` applies) after `actions::refuse_critical_change`. On top
//!   of that a rule can never name Orbit, Windows' own processes or anything
//!   under %SystemRoot%, Windows Security or anti-cheat software, and rows
//!   grouped as Windows are never touched.
//! - Realtime is rejected everywhere, and so is an affinity that leaves no core.
//! - A failure is recorded once per process instance with a plain reason and
//!   is not retried.
//! - This module never writes Image File Execution Options.
//!
//! Rules live in `rules.json` under `<app local data>\rules`, or under
//! `ORBIT_RULES_DIR` in debug builds. The log and the per-process status are
//! in memory only.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager};
use windows::core::w;
use windows::Win32::Foundation::{HMODULE, HWND};
use windows::Win32::System::Threading::{
    GetCurrentProcess, GetPriorityClass, GetProcessInformation, ProcessPowerThrottling,
    SetProcessInformation, PROCESS_POWER_THROTTLING_CURRENT_VERSION,
    PROCESS_POWER_THROTTLING_EXECUTION_SPEED, PROCESS_POWER_THROTTLING_STATE,
    PROCESS_QUERY_INFORMATION, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SET_INFORMATION,
};
use windows::Win32::UI::Accessibility::{SetWinEventHook, HWINEVENTHOOK};
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetForegroundWindow, GetMessageW, GetWindowThreadProcessId,
    EVENT_SYSTEM_FOREGROUND, MSG, WINEVENT_OUTOFCONTEXT,
};

use crate::actions::{self, ACCESS_DENIED, EXITED};
use crate::model::{ProcessKind, ProcessRow, Snapshot};

/// Sent when a process's rule status changes (applied, failed, stopped, gone).
pub const RULES_EVENT: &str = "orbit://rules";
const FILE_NAME: &str = "rules.json";
const BAD_FILE_NAME: &str = "rules.bad.json";
const MAX_RULES: usize = 200;
const MAX_LOG: usize = 200;
/// Reverts within this window make Orbit stop for that process.
const REVERT_WINDOW_MS: u64 = 60_000;
const REVERT_LIMIT: usize = 3;
/// Windows 11 starts at build 22000; efficiency mode is offered from there.
const WINDOWS_11_BUILD: u32 = 22_000;

pub const PRIORITIES: [&str; 5] = ["low", "belowNormal", "normal", "aboveNormal", "high"];
/// What an "only while in front" rule uses while the app is behind.
pub const BEHIND: [&str; 2] = ["normal", "belowNormal"];

pub const NEEDS_ADMIN: &str = "This app runs as administrator. Restart Orbit as administrator to change it.";
pub const BLOCKED: &str = "Windows or the app's protection blocked this change.";
pub const CHANGED_BACK: &str = "Kept being changed back by something else.";
const NO_REALTIME: &str = "Orbit never sets Realtime. It can freeze Windows.";
const NO_ECO: &str = "Efficiency mode needs Windows 11.";

const WHY_ORBIT: &str = "Orbit can't change itself.";
const WHY_WINDOWS: &str = "Windows needs this process. Orbit won't change it.";
const WHY_DEFENDER: &str = "Windows Security protects itself. Orbit won't change it.";
const WHY_ANTICHEAT: &str = "Anti-cheat software blocks changes like this. Orbit won't try.";

/// True while at least one enabled rule exists and the master switch is on,
/// so the sampler knows whether `light_tick` is worth calling.
static WANTS_TICKS: AtomicBool = AtomicBool::new(false);

// ---------------------------------------------------------------------------
// Shapes
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RuleMatch {
    /// "path" (full exe path) or "name" (exe file name, any folder).
    pub kind: String,
    pub value: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Rule {
    pub id: String,
    #[serde(rename = "match")]
    pub matcher: RuleMatch,
    /// "low" | "belowNormal" | "normal" | "aboveNormal" | "high".
    pub priority: Option<String>,
    /// Core mask as a decimal string (like `set_affinity`), `None` leaves it alone.
    pub affinity: Option<String>,
    /// Efficiency mode (Windows 11): EcoQoS plus Low priority, as Task Manager does.
    pub efficiency: bool,
    /// The priority applies only while the app's window is in front.
    pub in_front: bool,
    /// The priority while it is behind ("normal" or "belowNormal"); `in_front` only.
    pub behind: Option<String>,
    /// The rule applies only while Boost is on.
    pub boost_only: bool,
    pub enabled: bool,
}

impl Default for Rule {
    fn default() -> Self {
        Self {
            id: String::new(),
            matcher: RuleMatch { kind: "name".into(), value: String::new() },
            priority: None,
            affinity: None,
            efficiency: false,
            in_front: false,
            behind: None,
            boost_only: false,
            enabled: true,
        }
    }
}

impl Rule {
    fn mask(&self) -> Option<u64> {
        self.affinity.as_deref().and_then(|m| m.trim().parse().ok())
    }

    /// Same settings, ignoring the id (for imports).
    fn same_settings(&self, other: &Rule) -> bool {
        Rule { id: String::new(), ..self.clone() } == Rule { id: String::new(), ..other.clone() }
    }
}

#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Failure {
    pub at: u64,
    pub reason: String,
    pub needs_admin: bool,
}

#[derive(Serialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RuleStat {
    pub last_applied: Option<u64>,
    pub last_failed: Option<Failure>,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct RuleView {
    #[serde(flatten)]
    pub rule: Rule,
    /// Unix milliseconds.
    pub last_applied: Option<u64>,
    pub last_failed: Option<Failure>,
    /// Processes running right now that this rule covers.
    pub running: u32,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct RulesState {
    pub rules_enabled: bool,
    pub is_admin: bool,
    pub logical_cores: u32,
    /// Windows 11: efficiency mode can be offered.
    pub efficiency_supported: bool,
    pub rules: Vec<RuleView>,
}

/// "applied" | "blocked" | "reverted" | "reset".
#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LogEntry {
    pub at: u64,
    pub kind: String,
    pub rule_id: String,
    pub app: String,
    pub pid: u32,
    pub text: String,
    pub needs_admin: bool,
}

/// "applied" | "failed" | "gaveUp".
#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ProcStatus {
    pub pid: u32,
    pub start_time: u64,
    pub rule_id: String,
    pub state: String,
    /// "High priority, 4 cores".
    pub summary: String,
    pub reason: Option<String>,
    pub needs_admin: bool,
    pub at: u64,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
struct RulesEvent {
    statuses: Vec<ProcStatus>,
}

/// The `rules` part of Settings > Backup > Export, and what Import reads.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct RulesBackup {
    pub rules_enabled: Option<bool>,
    pub rules: Vec<Rule>,
}

/// What an import changes (or would change, with `apply: false`).
#[derive(Serialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ImportReport {
    pub added: u32,
    pub replaced: u32,
    pub unchanged: u32,
    /// "csrss.exe: Windows needs this process. Orbit won't change it."
    pub skipped: Vec<String>,
    /// The master switch value the file sets, when it differs from now.
    pub switch_to: Option<bool>,
}

// ---------------------------------------------------------------------------
// Which names a rule may never target
// ---------------------------------------------------------------------------

const WINDOWS_NAMES: &[&str] = &[
    "csrss.exe", "wininit.exe", "smss.exe", "lsass.exe", "lsaiso.exe", "services.exe",
    "winlogon.exe", "svchost.exe", "dwm.exe", "fontdrvhost.exe", "system",
    "system idle process", "registry", "secure system", "memory compression",
];
const DEFENDER_NAMES: &[&str] = &[
    "msmpeng.exe", "nissrv.exe", "mpdefendercoreservice.exe", "mpcmdrun.exe", "mssense.exe",
    "sensecncproxy.exe", "securityhealthservice.exe", "securityhealthsystray.exe", "sechealthui.exe",
];
/// Folders of Windows Security outside %SystemRoot% (Defender's platform updates,
/// the Windows Security app's package).
const DEFENDER_DIRS: &[&str] = &[r"\windows defender\", r"\windows defender advanced threat protection\", r"\microsoft.sechealthui_"];
const ANTICHEAT_NAMES: &[&str] = &["vgc.exe", "vgtray.exe"];
/// Exe name prefix and the product, for the refusal and the "blocked" message.
const ANTICHEAT_PREFIXES: &[(&str, &str)] = &[
    ("vgc", "Riot Vanguard"),
    ("vgtray", "Riot Vanguard"),
    ("vgk", "Riot Vanguard"),
    ("easyanticheat", "Easy Anti-Cheat"),
    ("eac_", "Easy Anti-Cheat"),
    ("beservice", "BattlEye"),
    ("bedaisy", "BattlEye"),
    ("battleye", "BattlEye"),
    ("eaanticheat", "EA Anti-Cheat"),
    ("equ8", "EQU8"),
    ("xigncode", "XIGNCODE3"),
    ("gameguard", "GameGuard"),
    ("npgg", "GameGuard"),
    ("faceit", "FACEIT"),
    ("pnkbstr", "PunkBuster"),
    ("sguard", "ACE anti-cheat"),
    ("ace-", "ACE anti-cheat"),
    ("mhyprot", "HoYoverse anti-cheat"),
];

/// File name part of a Windows path or a bare name.
fn file_name(s: &str) -> &str {
    s.trim().trim_matches('"').rsplit(['\\', '/']).next().unwrap_or("").trim()
}

/// Lowercase "%SystemRoot%\" (with the trailing backslash), read once.
fn system_root() -> &'static str {
    static ROOT: OnceLock<String> = OnceLock::new();
    ROOT.get_or_init(|| {
        let root = std::env::var("SystemRoot").or_else(|_| std::env::var("windir")).unwrap_or_else(|_| r"C:\Windows".into());
        format!("{}\\", actions::normalize_path(&root).trim_end_matches('\\'))
    })
}

/// The anti-cheat product an exe name belongs to.
fn anticheat_product(name: &str) -> Option<&'static str> {
    let lower = name.to_lowercase();
    if ANTICHEAT_NAMES.contains(&lower.as_str()) {
        return Some("Riot Vanguard");
    }
    ANTICHEAT_PREFIXES.iter().find(|(p, _)| lower.starts_with(p)).map(|(_, product)| *product)
}

/// Why `value` (an exe name or a full path) may not be the target of a rule.
pub fn refusal(value: &str) -> Option<&'static str> {
    let name = file_name(value).to_lowercase();
    if name == "orbit.exe" {
        return Some(WHY_ORBIT);
    }
    if WINDOWS_NAMES.contains(&name.as_str()) {
        return Some(WHY_WINDOWS);
    }
    if DEFENDER_NAMES.contains(&name.as_str()) {
        return Some(WHY_DEFENDER);
    }
    if anticheat_product(&name).is_some() {
        return Some(WHY_ANTICHEAT);
    }
    if value.contains(['\\', '/']) {
        let path = actions::normalize_path(value);
        if path.starts_with(system_root()) {
            return Some(WHY_WINDOWS);
        }
        if DEFENDER_DIRS.iter().any(|d| path.contains(d)) {
            return Some(WHY_DEFENDER);
        }
    }
    None
}

/// True when Windows itself ships an exe of this name (in %SystemRoot%,
/// System32 or SysWOW64). A name rule for it would only ever meet Windows'
/// copy, so it is refused up front. Only called when a rule is made or checked.
fn windows_ships(name: &str) -> bool {
    let name = file_name(name);
    !name.is_empty() && ["", "system32\\", "syswow64\\"].iter().any(|d| Path::new(&format!("{}{d}{name}", system_root())).is_file())
}

/// The refusal for a rule target: `refusal`, plus exe names Windows ships
/// for name rules.
pub fn rule_refusal(kind: &str, value: &str) -> Option<&'static str> {
    refusal(value).or_else(|| (kind == "name" && windows_ships(value)).then_some(WHY_WINDOWS))
}

/// A refusal for a running process: its path when known, else its name.
fn row_refused(row: &ProcessRow) -> bool {
    refusal(&row.name).is_some() || row.exe.as_deref().is_some_and(|p| refusal(p).is_some())
}

// ---------------------------------------------------------------------------
// Validation and matching
// ---------------------------------------------------------------------------

fn same_target(a: &RuleMatch, b: &RuleMatch) -> bool {
    a.kind == b.kind && key_of(a) == key_of(b)
}

fn key_of(m: &RuleMatch) -> String {
    if m.kind == "path" {
        actions::normalize_path(&m.value)
    } else {
        m.value.trim().to_lowercase()
    }
}

/// True when the rule covers a process named `name` at `exe` (when known).
pub fn rule_matches(rule: &Rule, name: &str, exe: Option<&str>) -> bool {
    if rule.matcher.kind == "path" {
        exe.is_some_and(|p| actions::normalize_path(p) == actions::normalize_path(&rule.matcher.value))
    } else {
        name.trim().eq_ignore_ascii_case(rule.matcher.value.trim())
    }
}

/// Whether a rule is in force right now.
fn active(rule: &Rule, boost_on: bool) -> bool {
    rule.enabled && (!rule.boost_only || boost_on)
}

/// The rule for a process: a path rule beats a name rule; otherwise the first.
fn find_rule<'a>(rules: &'a [Rule], row: &ProcessRow, boost_on: bool) -> Option<&'a Rule> {
    find_rule_for(rules, &row.name, row.exe.as_deref(), boost_on)
}

fn find_rule_for<'a>(rules: &'a [Rule], name: &str, exe: Option<&str>, boost_on: bool) -> Option<&'a Rule> {
    let exe = exe.map(actions::normalize_path);
    let hit = |r: &&Rule| {
        active(r, boost_on)
            && if r.matcher.kind == "path" {
                exe.as_deref().is_some_and(|e| e == actions::normalize_path(&r.matcher.value))
            } else {
                name.trim().eq_ignore_ascii_case(r.matcher.value.trim())
            }
    };
    rules.iter().filter(|r| r.matcher.kind == "path").find(hit).or_else(|| rules.iter().filter(|r| r.matcher.kind == "name").find(hit))
}

fn clean_priority(p: Option<&str>, allowed: &[&str]) -> Result<Option<String>, String> {
    match p.map(str::trim).filter(|p| !p.is_empty()) {
        None => Ok(None),
        Some("realtime") => Err(NO_REALTIME.into()),
        Some(p) if allowed.contains(&p) => Ok(Some(p.to_string())),
        Some(_) => Err("That priority is not valid.".into()),
    }
}

/// Checks a rule from the UI (or a file) and returns the cleaned version.
/// `others` are the existing rules (the one being edited excluded) for the
/// duplicate check. `eco` is whether this PC supports efficiency mode.
pub fn validate(input: &Rule, others: &[Rule], system_mask: u64, eco: bool) -> Result<Rule, String> {
    let kind = input.matcher.kind.trim();
    if kind != "path" && kind != "name" {
        return Err("Pick whether the rule matches a path or a name.".into());
    }
    let value = input.matcher.value.trim().trim_matches('"').trim().to_string();
    if value.is_empty() {
        return Err("Pick an app first.".into());
    }
    if value.len() > 520 || value.chars().any(|c| c.is_control()) {
        return Err("That app name is not valid.".into());
    }
    if !value.to_lowercase().ends_with(".exe") {
        return Err("Pick an .exe file.".into());
    }
    if kind == "name" && value.contains(['\\', '/']) {
        return Err("Use just the file name, like game.exe.".into());
    }
    if kind == "path" {
        let b = value.as_bytes();
        let drive = b.len() > 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == b'\\' || b[2] == b'/');
        if !drive && !value.starts_with(r"\\") {
            return Err("Use the full path, like C:\\Games\\game.exe.".into());
        }
    }
    if let Some(why) = rule_refusal(kind, &value) {
        return Err(why.into());
    }
    let mut priority = clean_priority(input.priority.as_deref(), &PRIORITIES)?;
    let affinity = match input.affinity.as_deref().map(str::trim).filter(|m| !m.is_empty()) {
        None => None,
        Some(m) => Some(actions::validate_mask(m, system_mask)?.to_string()),
    };
    let mut behind = None;
    if input.efficiency {
        if !eco {
            return Err(NO_ECO.into());
        }
        if input.in_front {
            return Err("Efficiency mode can't be used with \"Only while in front\".".into());
        }
        if priority.as_deref().is_some_and(|p| p != "low") {
            return Err("Efficiency mode runs the app at Low priority, so leave the priority as it is.".into());
        }
        // Efficiency mode sets Low itself.
        priority = None;
    }
    if input.in_front {
        let Some(front) = priority.as_deref() else {
            return Err("Pick a priority to use while the app is in front.".into());
        };
        let back = clean_priority(input.behind.as_deref(), &BEHIND)
            .map_err(|e| if e == NO_REALTIME { e } else { "Pick Normal or Below normal for when it's behind.".into() })?
            .unwrap_or_else(|| "normal".to_string());
        if back == front {
            return Err("Pick a different priority for when the app is in front.".into());
        }
        behind = Some(back);
    }
    if priority.is_none() && affinity.is_none() && !input.efficiency {
        return Err("Pick a priority or some processors to set.".into());
    }
    let rule = Rule {
        id: input.id.clone(),
        matcher: RuleMatch { kind: kind.to_string(), value },
        priority,
        affinity,
        efficiency: input.efficiency,
        in_front: input.in_front,
        behind,
        boost_only: input.boost_only,
        enabled: input.enabled,
    };
    if others.iter().any(|o| o.id != rule.id && same_target(&o.matcher, &rule.matcher)) {
        return Err("There is already a rule for that app.".into());
    }
    Ok(rule)
}

pub fn priority_label(p: &str) -> &'static str {
    match p {
        "low" => "Low",
        "belowNormal" => "Below normal",
        "normal" => "Normal",
        "aboveNormal" => "Above normal",
        "high" => "High",
        _ => "Unknown",
    }
}

fn cores_text(mask: u64) -> String {
    let n = mask.count_ones();
    format!("{n} {}", if n == 1 { "core" } else { "cores" })
}

/// "High priority, 4 cores", "High priority in front, Normal behind",
/// "Efficiency mode, while Boost is on".
pub fn summary(rule: &Rule) -> String {
    let mut parts = Vec::new();
    if rule.efficiency {
        parts.push("Efficiency mode".to_string());
    }
    if let Some(p) = &rule.priority {
        if rule.in_front {
            let back = rule.behind.as_deref().unwrap_or("normal");
            parts.push(format!("{} priority in front, {} behind", priority_label(p), priority_label(back)));
        } else {
            parts.push(format!("{} priority", priority_label(p)));
        }
    }
    if let Some(m) = rule.mask() {
        parts.push(cores_text(m));
    }
    if rule.boost_only {
        parts.push("while Boost is on".to_string());
    }
    parts.join(", ")
}

// ---------------------------------------------------------------------------
// What a process should have
// ---------------------------------------------------------------------------

/// The values a rule asks for right now. `None` leaves that value alone.
#[derive(Clone, Debug, PartialEq)]
pub struct Want {
    pub priority: Option<&'static str>,
    pub mask: Option<u64>,
    /// Efficiency mode on (Some(true)) or off (Some(false), used by Reset).
    pub eco: Option<bool>,
}

fn canon(p: &str) -> Option<&'static str> {
    PRIORITIES.iter().find(|x| **x == p).copied()
}

/// What `rule` asks of a process that is (or is not) in front.
pub fn want_of(rule: &Rule, in_front: bool) -> Want {
    if rule.efficiency {
        return Want { priority: Some("low"), mask: rule.mask(), eco: Some(true) };
    }
    let priority = match (rule.priority.as_deref(), rule.in_front) {
        (Some(_), true) if !in_front => canon(rule.behind.as_deref().unwrap_or("normal")).or(Some("normal")),
        (Some(p), _) => canon(p),
        (None, _) => None,
    };
    Want { priority, mask: rule.mask(), eco: None }
}

/// What Reset (and Boost ending) puts back: Normal priority, every core,
/// efficiency mode off, for the values the rule sets.
pub fn reset_want(rule: &Rule, system_mask: u64) -> Want {
    Want {
        priority: (rule.priority.is_some() || rule.efficiency).then_some("normal"),
        mask: rule.affinity.is_some().then_some(system_mask),
        eco: rule.efficiency.then_some(false),
    }
}

/// "High priority, 2 cores" for a `Want`.
fn want_text(w: &Want) -> String {
    let mut parts = Vec::new();
    if w.eco == Some(true) {
        parts.push("Efficiency mode".to_string());
    } else if let Some(p) = w.priority {
        parts.push(format!("{} priority", priority_label(p)));
    }
    if let Some(m) = w.mask {
        parts.push(cores_text(m));
    }
    parts.join(", ")
}

/// "Normal priority and all cores" for what Reset puts back.
fn reset_text(w: &Want) -> String {
    let mut parts = Vec::new();
    if w.priority.is_some() {
        parts.push("Normal priority");
    }
    if w.mask.is_some() {
        parts.push("all cores");
    }
    if w.eco == Some(false) {
        parts.push("efficiency mode off");
    }
    match parts.len() {
        0 => String::new(),
        1 => parts[0].to_string(),
        n => format!("{} and {}", parts[..n - 1].join(", "), parts[n - 1]),
    }
}

/// What a process has now. `eco` is `None` when it can't be read.
#[derive(Clone, Debug, PartialEq)]
pub struct Current {
    pub priority: String,
    pub mask: u64,
    pub eco: Option<bool>,
}

/// What differs between what Orbit set (`last`) and what the process has now,
/// in plain words. Empty when nothing was changed back.
fn drift(last: &Want, cur: &Current) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(p) = last.priority {
        if cur.priority != p {
            out.push(format!("The priority was changed to {}.", priority_label(&cur.priority)));
        }
    }
    if let Some(m) = last.mask {
        if cur.mask != m {
            out.push("The processors were changed.".to_string());
        }
    }
    if let (Some(want), Some(now)) = (last.eco, cur.eco) {
        if want != now {
            out.push(if want { "Efficiency mode was turned off." } else { "Efficiency mode was turned on." }.to_string());
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Reverts
// ---------------------------------------------------------------------------

#[derive(Debug, PartialEq, Eq)]
pub enum Revert {
    /// Set the value again.
    Reapply,
    /// Third time inside the window: stop for this process.
    GiveUp,
}

/// Records that something changed a value back at `now` (ms) and says what to do.
pub fn note_revert(reverts: &mut Vec<u64>, now: u64) -> Revert {
    reverts.retain(|&t| now.saturating_sub(t) < REVERT_WINDOW_MS);
    reverts.push(now);
    if reverts.len() >= REVERT_LIMIT {
        Revert::GiveUp
    } else {
        Revert::Reapply
    }
}

// ---------------------------------------------------------------------------
// Engine
// ---------------------------------------------------------------------------

type Key = (u32, u64);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum State {
    Applied,
    Failed,
    GaveUp,
}

struct Tracked {
    rule_id: String,
    name: String,
    exe: Option<String>,
    state: State,
    summary: String,
    reason: Option<String>,
    needs_admin: bool,
    at: u64,
    reverts: Vec<u64>,
    /// What Orbit set last. A difference from this is a revert; a new `Want`
    /// (the window moved, Boost ended) is not.
    last: Want,
}

/// A process seen by `light_tick`: name, parent, start time (Unix s) and
/// path. `ok` is false when it could not be opened (not tried again while
/// the pid, name and parent stay the same).
struct LightProc {
    name: String,
    parent: u32,
    start: u64,
    exe: Option<String>,
    ok: bool,
}

/// What Boost is doing this tick: on or off, and the processes it changed
/// (left alone by the rules while Boost holds them).
#[derive(Clone, Debug, Default)]
pub struct BoostNow {
    pub on: bool,
    pub touched: HashSet<Key>,
}

impl BoostNow {
    fn current() -> Self {
        Self { on: crate::boost::is_active(), touched: crate::boost::touched() }
    }
}

pub struct Engine {
    dir: Option<PathBuf>,
    initialized: bool,
    enabled: bool,
    rules: Vec<Rule>,
    tracked: HashMap<Key, Tracked>,
    /// Running processes already compared with the rules and found without one.
    checked: HashSet<Key>,
    log: VecDeque<LogEntry>,
    stats: HashMap<String, RuleStat>,
    admin: bool,
    system_mask: u64,
    eco_supported: bool,
    next_id: u64,
    /// Pid of the window in front (0 when unknown).
    foreground: u32,
    /// Boost's state at the last tick, to re-check processes when it changes.
    boost_seen: bool,
    light: HashMap<u32, LightProc>,
}

impl Engine {
    pub fn new(admin: bool, system_mask: u64, eco_supported: bool) -> Self {
        Self {
            dir: None,
            initialized: false,
            enabled: true,
            rules: Vec::new(),
            tracked: HashMap::new(),
            checked: HashSet::new(),
            log: VecDeque::new(),
            stats: HashMap::new(),
            admin,
            system_mask,
            eco_supported,
            next_id: 0,
            foreground: 0,
            boost_seen: false,
            light: HashMap::new(),
        }
    }

    fn push_log(&mut self, entry: LogEntry) {
        self.log.push_back(entry);
        while self.log.len() > MAX_LOG {
            self.log.pop_front();
        }
    }

    fn applied(&mut self, rule_id: &str, now: u64) {
        self.stats.entry(rule_id.to_string()).or_default().last_applied = Some(now);
    }

    fn failed(&mut self, rule_id: &str, now: u64, reason: &str, needs_admin: bool) {
        self.stats.entry(rule_id.to_string()).or_default().last_failed =
            Some(Failure { at: now, reason: reason.to_string(), needs_admin });
    }

    /// After the rules changed (or Boost turned on or off): forget which
    /// processes had no rule, so every one is compared again, and drop the
    /// tracked processes whose rule changed or no longer wins. A process whose
    /// rule is unchanged keeps its state: one Orbit gave up on stays given up.
    fn retrack(&mut self, before: &[Rule]) -> bool {
        self.checked.clear();
        let n = self.tracked.len();
        let rules = &self.rules;
        let boost = self.boost_seen;
        self.tracked.retain(|_, t| {
            let unchanged = before.iter().find(|r| r.id == t.rule_id) == rules.iter().find(|r| r.id == t.rule_id);
            unchanged && find_rule_for(rules, &t.name, t.exe.as_deref(), boost).is_some_and(|w| w.id == t.rule_id)
        });
        n != self.tracked.len()
    }

    /// Forgets everything learned about processes (tests).
    #[cfg(test)]
    fn reset_tracking(&mut self) -> bool {
        self.checked.clear();
        let had = !self.tracked.is_empty();
        self.tracked.clear();
        had
    }

    /// Keeps `WANTS_TICKS` in step with the rules and the master switch.
    fn publish(&self) {
        WANTS_TICKS.store(self.initialized && self.enabled && self.rules.iter().any(|r| r.enabled), Ordering::Relaxed);
    }

    fn new_id(&mut self, now: u64) -> String {
        self.next_id += 1;
        format!("r{:x}{:x}", now, self.next_id)
    }

    /// The rule of the process in front, when it has one: every process of
    /// that rule counts as in front (an app can have several processes).
    fn front_rule(&self) -> Option<&str> {
        if self.foreground == 0 {
            return None;
        }
        self.tracked.iter().find(|(k, _)| k.0 == self.foreground).map(|(_, t)| t.rule_id.as_str())
    }

    fn is_front(&self, key: Key, rule_id: &str) -> bool {
        key.0 == self.foreground || self.front_rule() == Some(rule_id)
    }

    pub fn statuses(&self) -> Vec<ProcStatus> {
        let mut out: Vec<ProcStatus> = self
            .tracked
            .iter()
            .map(|(k, t)| ProcStatus {
                pid: k.0,
                start_time: k.1,
                rule_id: t.rule_id.clone(),
                state: match t.state {
                    State::Applied => "applied",
                    State::Failed => "failed",
                    State::GaveUp => "gaveUp",
                }
                .to_string(),
                summary: t.summary.clone(),
                reason: t.reason.clone(),
                needs_admin: t.needs_admin,
                at: t.at,
            })
            .collect();
        out.sort_by_key(|s| s.pid);
        out
    }
}

/// What the engine needs from Windows (a fake in unit tests).
pub trait Ops {
    /// Refuses Orbit, its helpers and processes Windows needs.
    fn guard(&self, pid: u32) -> Result<(), String>;
    /// Priority ("low" for Idle; "realtime" possible), affinity mask and,
    /// when `eco` is true, efficiency mode.
    fn read(&self, pid: u32, start: u64, eco: bool) -> Result<Current, String>;
    fn set_priority(&self, pid: u32, start: u64, name: &str) -> Result<(), String>;
    fn set_affinity(&self, pid: u32, start: u64, mask: u64) -> Result<(), String>;
    fn set_eco(&self, pid: u32, start: u64, on: bool) -> Result<(), String>;
    /// Whether the process runs as administrator, when that can be read.
    fn elevated(&self, pid: u32) -> Option<bool>;
}

/// Turns an error from `actions` into the text the owner sees. `None` means the
/// process is gone and nothing should be said. `anticheat` is the anti-cheat
/// product running on this PC, if any.
fn explain(err: &str, admin: bool, target_elevated: Option<bool>, anticheat: Option<&str>) -> Option<(String, bool)> {
    if err == EXITED {
        return None;
    }
    if err != ACCESS_DENIED {
        return Some((err.to_string(), false));
    }
    if !admin && target_elevated == Some(true) {
        return Some((NEEDS_ADMIN.to_string(), true));
    }
    if let Some(product) = anticheat {
        return Some((format!("{product} (anti-cheat) blocked this change. Orbit won't keep trying."), false));
    }
    if admin || target_elevated == Some(false) {
        return Some((BLOCKED.to_string(), false));
    }
    Some((NEEDS_ADMIN.to_string(), true))
}

/// The anti-cheat product among the running processes, if any.
fn anticheat_running(rows: &[ProcessRow]) -> Option<&'static str> {
    rows.iter().find_map(|r| anticheat_product(&r.name))
}

/// Sets what differs from `want`. Returns whether anything was changed.
fn apply(want: &Want, pid: u32, start: u64, ops: &dyn Ops) -> Result<bool, String> {
    if want.priority == Some("realtime") {
        return Err(NO_REALTIME.into());
    }
    ops.guard(pid)?;
    let cur = ops.read(pid, start, want.eco.is_some())?;
    let mut changed = false;
    if let Some(p) = want.priority {
        if cur.priority != p {
            ops.set_priority(pid, start, p)?;
            changed = true;
        }
    }
    if let Some(m) = want.mask {
        if cur.mask != m {
            ops.set_affinity(pid, start, m)?;
            changed = true;
        }
    }
    if let Some(on) = want.eco {
        if cur.eco != Some(on) {
            ops.set_eco(pid, start, on)?;
            changed = true;
        }
    }
    Ok(changed)
}

fn app_log(kind: &str, rule_id: &str, app: &str, pid: u32, now: u64, text: String, needs_admin: bool) -> LogEntry {
    LogEntry { at: now, kind: kind.into(), rule_id: rule_id.to_string(), app: app.to_string(), pid, text, needs_admin }
}

/// " (in front)" / " (behind)" for "only while in front" rules.
fn front_note(rule: &Rule, front: bool) -> &'static str {
    match (rule.in_front, front) {
        (true, true) => " It's in front.",
        (true, false) => " It's behind.",
        _ => "",
    }
}

/// One tick: new processes get their rule, tracked ones are re-checked.
/// Returns true when a status changed (so the UI should hear about it).
pub fn step(e: &mut Engine, rows: &[ProcessRow], now: u64, ops: &dyn Ops, own_pid: u32, boost: &BoostNow) -> bool {
    let boost_on = boost.on;
    if !e.enabled || !e.rules.iter().any(|r| r.enabled) {
        e.checked.clear();
        e.boost_seen = boost_on;
        let had = !e.tracked.is_empty();
        e.tracked.clear();
        return had;
    }
    let mut changed = false;
    let boost_flipped = boost_on != e.boost_seen;
    e.boost_seen = boost_on;

    // Processes that ended.
    if !e.tracked.is_empty() || e.checked.len() > rows.len() + 64 {
        let live: HashSet<Key> = rows.iter().map(|r| (r.pid, r.start_time)).collect();
        let before = e.tracked.len();
        e.tracked.retain(|k, _| live.contains(k));
        changed |= e.tracked.len() != before;
        if e.checked.len() > rows.len() + 64 {
            e.checked.retain(|k| live.contains(k));
        }
    }

    // Rules no longer in force (Boost ended): put back what they set.
    let released: Vec<Key> = e
        .tracked
        .iter()
        .filter(|(_, t)| e.rules.iter().find(|r| r.id == t.rule_id).is_none_or(|r| !active(r, boost_on)))
        .map(|(k, _)| *k)
        .collect();
    for key in released {
        // Not put in `checked`: below, the process gets whichever rule is in force now.
        let t = e.tracked.remove(&key).expect("tracked");
        changed = true;
        let Some(rule) = e.rules.iter().find(|r| r.id == t.rule_id).cloned() else { continue };
        if t.state != State::Applied || !rule.enabled || !rule.boost_only {
            continue;
        }
        let back = reset_want(&rule, e.system_mask);
        match apply(&back, key.0, key.1, ops) {
            Ok(_) => e.push_log(app_log("reset", &rule.id, &t.name, key.0, now, format!("Boost ended. Put back to {}.", reset_text(&back)), false)),
            Err(err) => {
                if let Some((reason, needs_admin)) = explain(&err, e.admin, ops.elevated(key.0), None) {
                    e.push_log(app_log("blocked", &rule.id, &t.name, key.0, now, reason, needs_admin));
                }
            }
        }
    }

    if boost_flipped {
        // Boost-only rules start or stop: compare everything again, and a
        // process whose winning rule changed gets the new one.
        let rules = e.rules.clone();
        changed |= e.retrack(&rules);
    }

    // New processes. Most have no rule: two hash lookups and a name compare.
    let mut anticheat: Option<Option<&'static str>> = None;
    for row in rows {
        let key = (row.pid, row.start_time);
        if e.tracked.contains_key(&key) || e.checked.contains(&key) || boost.touched.contains(&key) {
            // Boost holds it for now; it is compared again once Boost lets go.
            continue;
        }
        if row.pid <= 4 || row.pid == own_pid || row.kind == ProcessKind::Windows {
            e.checked.insert(key);
            continue;
        }
        let Some(rule) = find_rule(&e.rules, row, boost_on).cloned() else {
            e.checked.insert(key);
            continue;
        };
        if row_refused(row) {
            e.checked.insert(key);
            continue;
        }
        let front = e.is_front(key, &rule.id);
        let want = want_of(&rule, front);
        let sum = summary(&rule);
        match apply(&want, row.pid, row.start_time, ops) {
            Ok(did) => {
                let text = if did { format!("Set to {}.", want_text(&want)) } else { format!("Already set to {}.", want_text(&want)) };
                e.push_log(app_log("applied", &rule.id, &row.name, row.pid, now, format!("{text}{}", front_note(&rule, front)), false));
                e.applied(&rule.id, now);
                e.tracked.insert(key, Tracked { rule_id: rule.id, name: row.name.clone(), exe: row.exe.clone(), state: State::Applied, summary: sum, reason: None, needs_admin: false, at: now, reverts: Vec::new(), last: want });
                changed = true;
            }
            Err(err) => {
                let ac = *anticheat.get_or_insert_with(|| anticheat_running(rows));
                match explain(&err, e.admin, ops.elevated(row.pid), ac) {
                    None => {
                        e.checked.insert(key);
                    }
                    Some((reason, needs_admin)) => {
                        e.push_log(app_log("blocked", &rule.id, &row.name, row.pid, now, reason.clone(), needs_admin));
                        e.failed(&rule.id, now, &reason, needs_admin);
                        e.tracked.insert(key, Tracked { rule_id: rule.id, name: row.name.clone(), exe: row.exe.clone(), state: State::Failed, summary: sum, reason: Some(reason), needs_admin, at: now, reverts: Vec::new(), last: want });
                        changed = true;
                    }
                }
            }
        }
    }

    // Re-check the ones that hold a rule.
    // A process Boost changed is Boost's until it turns off (and puts it back).
    let keys: Vec<Key> = e.tracked.iter().filter(|(k, t)| t.state == State::Applied && !boost.touched.contains(k)).map(|(k, _)| *k).collect();
    for key in keys {
        let Some(rule) = e.tracked.get(&key).and_then(|t| e.rules.iter().find(|r| r.id == t.rule_id)).cloned() else { continue };
        let front = e.is_front(key, &rule.id);
        let want = want_of(&rule, front);
        let (last, name) = {
            let t = &e.tracked[&key];
            (t.last.clone(), t.name.clone())
        };
        // A failed read is skipped: it worked when the rule was applied.
        let cur = match ops.read(key.0, key.1, last.eco.is_some()) {
            Ok(cur) => cur,
            Err(err) if err == EXITED => {
                e.tracked.remove(&key);
                changed = true;
                continue;
            }
            Err(_) => continue,
        };
        let moved = drift(&last, &cur);
        if moved.is_empty() && want == last {
            continue;
        }
        if !moved.is_empty() {
            changed = true;
            let verdict = note_revert(&mut e.tracked.get_mut(&key).expect("tracked").reverts, now);
            if verdict == Revert::GiveUp {
                let t = e.tracked.get_mut(&key).expect("tracked");
                t.state = State::GaveUp;
                t.reason = Some(CHANGED_BACK.to_string());
                t.at = now;
                e.push_log(app_log("reverted", &rule.id, &name, key.0, now, format!("{CHANGED_BACK} Orbit stopped for this run."), false));
                e.failed(&rule.id, now, CHANGED_BACK, false);
                continue;
            }
        }
        match apply(&want, key.0, key.1, ops) {
            Ok(_) => {
                if !moved.is_empty() {
                    e.push_log(app_log("reverted", &rule.id, &name, key.0, now, format!("{} Orbit set {} again.", moved.join(" "), want_text(&want)), false));
                    e.applied(&rule.id, now);
                }
                if let Some(t) = e.tracked.get_mut(&key) {
                    t.at = now;
                    t.last = want;
                }
            }
            Err(err) => match explain(&err, e.admin, ops.elevated(key.0), anticheat_running(rows)) {
                None => {
                    e.tracked.remove(&key);
                    changed = true;
                }
                Some((reason, needs_admin)) => {
                    changed = true;
                    e.push_log(app_log("blocked", &rule.id, &name, key.0, now, reason.clone(), needs_admin));
                    e.failed(&rule.id, now, &reason, needs_admin);
                    if let Some(t) = e.tracked.get_mut(&key) {
                        t.state = State::Failed;
                        t.reason = Some(reason);
                        t.needs_admin = needs_admin;
                        t.at = now;
                    }
                }
            },
        }
    }
    changed
}

/// The window in front moved to `pid`: processes of "only while in front"
/// rules that just moved to the front or the back get their priority now,
/// without waiting for the next tick. Not a revert. Returns true when a
/// status changed.
pub fn front_changed(e: &mut Engine, pid: u32, now: u64, ops: &dyn Ops, boost_held: &HashSet<Key>) -> bool {
    if pid == e.foreground {
        return false;
    }
    let old_rule = e.front_rule().map(str::to_string);
    let old_pid = e.foreground;
    e.foreground = pid;
    if !e.enabled || !e.rules.iter().any(|r| r.enabled && r.in_front) {
        return false;
    }
    let new_rule = e.front_rule().map(str::to_string);
    let touched: Vec<Key> = e
        .tracked
        .iter()
        .filter(|(k, t)| {
            t.state == State::Applied
                && !boost_held.contains(k)
                && (k.0 == old_pid || k.0 == pid || Some(&t.rule_id) == old_rule.as_ref() || Some(&t.rule_id) == new_rule.as_ref())
        })
        .map(|(k, _)| *k)
        .collect();
    let mut changed = false;
    for key in touched {
        let Some(rule) = e.tracked.get(&key).and_then(|t| e.rules.iter().find(|r| r.id == t.rule_id)).cloned() else { continue };
        if !rule.in_front {
            continue;
        }
        let want = want_of(&rule, e.is_front(key, &rule.id));
        let last = e.tracked[&key].last.clone();
        let (Some(p), true) = (want.priority, want.priority != last.priority) else { continue };
        match ops.set_priority(key.0, key.1, p) {
            Ok(()) => {
                if let Some(t) = e.tracked.get_mut(&key) {
                    t.last = want;
                    t.at = now;
                }
            }
            Err(err) => {
                let name = e.tracked[&key].name.clone();
                if let Some((reason, needs_admin)) = explain(&err, e.admin, ops.elevated(key.0), None) {
                    e.push_log(app_log("blocked", &rule.id, &name, key.0, now, reason.clone(), needs_admin));
                    e.failed(&rule.id, now, &reason, needs_admin);
                    if let Some(t) = e.tracked.get_mut(&key) {
                        t.state = State::Failed;
                        t.reason = Some(reason);
                        t.needs_admin = needs_admin;
                        t.at = now;
                    }
                    changed = true;
                }
            }
        }
    }
    changed
}

/// Reset for one rule: running processes it covers go back to Normal
/// priority, every core and efficiency mode off (only what the rule sets),
/// and the rule is switched off so it doesn't apply again. Returns how many
/// processes were put back and the problems, one line each.
pub fn reset_rule(e: &mut Engine, id: &str, rows: &[ProcessRow], now: u64, ops: &dyn Ops, own_pid: u32) -> Result<(u32, Vec<String>), String> {
    let Some(pos) = e.rules.iter().position(|r| r.id == id) else {
        return Err("That rule no longer exists.".into());
    };
    let rule = e.rules[pos].clone();
    let back = reset_want(&rule, e.system_mask);
    // Only the processes this rule holds (or would hold, when it is off):
    // another rule that wins for a process keeps it.
    let mut probe = e.rules.clone();
    probe[pos].enabled = true;
    probe[pos].boost_only = false;
    let mut done = 0;
    let mut problems = Vec::new();
    for row in rows.iter().filter(|r| r.pid > 4 && r.pid != own_pid && r.kind != ProcessKind::Windows) {
        if find_rule(&probe, row, e.boost_seen).is_none_or(|w| w.id != rule.id) || row_refused(row) {
            continue;
        }
        match apply(&back, row.pid, row.start_time, ops) {
            Ok(_) => {
                done += 1;
                e.push_log(app_log("reset", &rule.id, &row.name, row.pid, now, format!("Put back to {}. The rule is off.", reset_text(&back)), false));
            }
            Err(err) => {
                if let Some((reason, needs_admin)) = explain(&err, e.admin, ops.elevated(row.pid), anticheat_running(rows)) {
                    e.push_log(app_log("blocked", &rule.id, &row.name, row.pid, now, reason.clone(), needs_admin));
                    problems.push(format!("{}: {reason}", row.name));
                }
            }
        }
    }
    e.rules[pos].enabled = false;
    e.tracked.retain(|_, t| t.rule_id != rule.id);
    e.checked.clear();
    Ok((done, problems))
}

/// Merges rules from a backup into `existing`: a rule for an app that already
/// has one replaces its settings (the id stays), the rest are added. Invalid
/// or refused rules are skipped with the reason.
pub fn merge(existing: &[Rule], incoming: &[Rule], system_mask: u64, eco: bool, mut new_id: impl FnMut() -> String) -> (Vec<Rule>, ImportReport) {
    let mut out = existing.to_vec();
    let mut report = ImportReport::default();
    let mut seen: Vec<RuleMatch> = Vec::new();
    for raw in incoming {
        let label = file_name(&raw.matcher.value).to_string();
        let label = if label.is_empty() { "A rule".to_string() } else { label };
        let probe = Rule { id: String::new(), ..raw.clone() };
        let clean = match validate(&probe, &[], system_mask, eco) {
            Ok(c) => c,
            Err(why) => {
                report.skipped.push(format!("{label}: {why}"));
                continue;
            }
        };
        if seen.iter().any(|m| same_target(m, &clean.matcher)) {
            report.skipped.push(format!("{label}: That file has two rules for this app; the first was used."));
            continue;
        }
        seen.push(clean.matcher.clone());
        match out.iter().position(|r| same_target(&r.matcher, &clean.matcher)) {
            Some(i) => {
                if out[i].same_settings(&clean) {
                    report.unchanged += 1;
                } else {
                    out[i] = Rule { id: out[i].id.clone(), ..clean };
                    report.replaced += 1;
                }
            }
            None if out.len() >= MAX_RULES => report.skipped.push(format!("{label}: Orbit keeps at most {MAX_RULES} rules.")),
            None => {
                out.push(Rule { id: new_id(), ..clean });
                report.added += 1;
            }
        }
    }
    (out, report)
}

// ---------------------------------------------------------------------------
// Real OS calls
// ---------------------------------------------------------------------------

pub struct RealOps;

impl RealOps {
    fn open(pid: u32, start: u64, access: windows::Win32::System::Threading::PROCESS_ACCESS_RIGHTS) -> Result<actions::OwnedHandle, String> {
        let h = actions::open_process(pid, access)?;
        actions::verify_identity(h.0, Some(start))?;
        Ok(h)
    }
}

impl Ops for RealOps {
    fn guard(&self, pid: u32) -> Result<(), String> {
        actions::guard_pid(pid, "changed")?;
        actions::refuse_critical_change(pid)
    }

    fn read(&self, pid: u32, start: u64, eco: bool) -> Result<Current, String> {
        // Reading power throttling may need full query rights; fall back to limited.
        let h = match eco {
            true => Self::open(pid, start, PROCESS_QUERY_INFORMATION | PROCESS_QUERY_LIMITED_INFORMATION)
                .or_else(|_| Self::open(pid, start, PROCESS_QUERY_LIMITED_INFORMATION))?,
            false => Self::open(pid, start, PROCESS_QUERY_LIMITED_INFORMATION)?,
        };
        let class = unsafe { GetPriorityClass(h.0) };
        if class == 0 {
            let err = windows::core::Error::from_win32();
            return Err(actions::describe(&err, "read the priority"));
        }
        let priority = match actions::priority_name(class) {
            Some("idle") => "low".to_string(),
            Some(name) => name.to_string(),
            None => "unknown".to_string(),
        };
        let (mask, _) = actions::read_affinity(h.0)?;
        let eco = if eco {
            let mut state = PROCESS_POWER_THROTTLING_STATE { Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION, ..Default::default() };
            unsafe {
                GetProcessInformation(h.0, ProcessPowerThrottling, (&mut state as *mut PROCESS_POWER_THROTTLING_STATE).cast(), size_of::<PROCESS_POWER_THROTTLING_STATE>() as u32)
            }
            .ok()
            .map(|_| {
                let bit = PROCESS_POWER_THROTTLING_EXECUTION_SPEED;
                state.ControlMask & bit != 0 && state.StateMask & bit != 0
            })
        } else {
            None
        };
        Ok(Current { priority, mask, eco })
    }

    fn set_priority(&self, pid: u32, start: u64, name: &str) -> Result<(), String> {
        let name = match name {
            "low" => "idle",
            "realtime" => return Err(NO_REALTIME.into()),
            other => other,
        };
        actions::set_priority(pid, name.to_string(), Some(start))
    }

    fn set_affinity(&self, pid: u32, start: u64, mask: u64) -> Result<(), String> {
        actions::set_affinity(pid, mask.to_string(), Some(start))
    }

    /// On: EcoQoS (execution speed throttled), as Task Manager's efficiency
    /// mode. Off: Windows decides again (no explicit setting).
    fn set_eco(&self, pid: u32, start: u64, on: bool) -> Result<(), String> {
        actions::guard_pid(pid, "changed")?;
        let h = Self::open(pid, start, PROCESS_SET_INFORMATION | PROCESS_QUERY_LIMITED_INFORMATION)?;
        let bit = if on { PROCESS_POWER_THROTTLING_EXECUTION_SPEED } else { 0 };
        let state = PROCESS_POWER_THROTTLING_STATE { Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION, ControlMask: bit, StateMask: bit };
        unsafe {
            SetProcessInformation(h.0, ProcessPowerThrottling, (&state as *const PROCESS_POWER_THROTTLING_STATE).cast(), size_of::<PROCESS_POWER_THROTTLING_STATE>() as u32)
        }
        .map_err(|e| actions::describe(&e, "change efficiency mode"))
    }

    fn elevated(&self, pid: u32) -> Option<bool> {
        actions::process_elevated(pid)
    }
}

/// Windows 11 (build 22000 or later).
fn is_windows_11() -> bool {
    const NT: &str = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion";
    crate::rig::reg_sz(NT, "CurrentBuildNumber")
        .or_else(|| crate::rig::reg_sz(NT, "CurrentBuild"))
        .and_then(|b| b.trim().parse::<u32>().ok())
        .is_some_and(|b| b >= WINDOWS_11_BUILD)
}

// ---------------------------------------------------------------------------
// Files
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct Stored {
    #[serde(rename = "rulesEnabled")]
    rules_enabled: Option<bool>,
    rules: Vec<Rule>,
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// `ORBIT_RULES_DIR` (debug builds only), else `<app local data>\rules`.
fn rules_dir(local_data: Option<PathBuf>) -> Option<PathBuf> {
    #[cfg(debug_assertions)]
    if let Some(v) = std::env::var_os("ORBIT_RULES_DIR").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(v));
    }
    local_data.map(|d| d.join("rules"))
}

/// Reads the rules file, dropping anything that is no longer valid. Each rule
/// is read on its own, so one bad value loses that rule only. A file that
/// isn't JSON at all is kept as `rules.bad.json` instead of being overwritten.
fn load(dir: &Path, system_mask: u64, eco: bool) -> (bool, Vec<Rule>) {
    let path = dir.join(FILE_NAME);
    let Ok(bytes) = fs::read(&path) else {
        return (true, Vec::new());
    };
    let value: serde_json::Value = match serde_json::from_slice(&bytes) {
        Ok(v) => v,
        Err(_) => {
            let _ = fs::rename(&path, dir.join(BAD_FILE_NAME));
            return (true, Vec::new());
        }
    };
    let enabled = value.get("rulesEnabled").and_then(serde_json::Value::as_bool).unwrap_or(true);
    let list = value.get("rules").and_then(serde_json::Value::as_array).cloned().unwrap_or_default();
    let mut rules: Vec<Rule> = Vec::new();
    for item in list {
        let Ok(r) = serde_json::from_value::<Rule>(item) else { continue };
        if r.id.is_empty() || rules.iter().any(|o| o.id == r.id) {
            continue;
        }
        if let Ok(clean) = validate(&r, &rules, system_mask, eco) {
            rules.push(clean);
        }
        if rules.len() == MAX_RULES {
            break;
        }
    }
    (enabled, rules)
}

fn save(e: &Engine) -> Result<(), String> {
    e.publish();
    let Some(dir) = e.dir.as_deref() else {
        return Ok(());
    };
    fs::create_dir_all(dir).map_err(|err| err.to_string())?;
    let stored = Stored { rules_enabled: Some(e.enabled), rules: e.rules.clone() };
    let bytes = serde_json::to_vec_pretty(&stored).map_err(|err| err.to_string())?;
    let path = dir.join(FILE_NAME);
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, bytes).map_err(|err| err.to_string())?;
    fs::rename(&tmp, path).map_err(|err| err.to_string())
}

// ---------------------------------------------------------------------------
// Global engine, hooks
// ---------------------------------------------------------------------------

fn engine() -> MutexGuard<'static, Engine> {
    static ENGINE: OnceLock<Mutex<Engine>> = OnceLock::new();
    ENGINE
        .get_or_init(|| {
            let system = actions::read_affinity(unsafe { GetCurrentProcess() }).map(|(_, s)| s).unwrap_or(1);
            let admin = unsafe { windows::Win32::UI::Shell::IsUserAnAdmin() }.as_bool();
            Mutex::new(Engine::new(admin, system, is_windows_11()))
        })
        .lock()
        .unwrap_or_else(|p| p.into_inner())
}

static APP: OnceLock<AppHandle> = OnceLock::new();

/// Setup hook: loads the saved rules and starts the foreground listener.
pub fn init(app: &AppHandle) {
    let _ = APP.set(app.clone());
    let dir = rules_dir(app.path().app_local_data_dir().ok());
    {
        let mut e = engine();
        if let Some(d) = dir.as_deref() {
            let (enabled, rules) = load(d, e.system_mask, e.eco_supported);
            e.enabled = enabled;
            e.rules = rules;
        }
        e.dir = dir;
        e.initialized = true;
        e.publish();
    }
    start_foreground_hook();
}

fn emit_status(app: &AppHandle) {
    let statuses = engine().statuses();
    let _ = app.emit(RULES_EVENT, RulesEvent { statuses });
}

fn on_rows(app: &AppHandle, rows: &[ProcessRow]) {
    // Read before taking the rules lock: the two locks are never held together.
    let boost = BoostNow::current();
    let changed = {
        let mut e = engine();
        e.initialized && step(&mut e, rows, now_ms(), &RealOps, std::process::id(), &boost)
    };
    if changed {
        emit_status(app);
    }
}

/// Called by the sampler after every snapshot.
pub fn on_snapshot(app: &AppHandle, snapshot: &Snapshot) {
    on_rows(app, &snapshot.processes);
}

/// True when the sampler should call `light_tick` while it isn't sampling.
pub fn wants_ticks() -> bool {
    WANTS_TICKS.load(Ordering::Relaxed)
}

/// A tick without a full sample (the sampler is paused or slower than a
/// second): see `fresh_rows`.
pub fn light_tick(app: &AppHandle) {
    let rows = fresh_rows(&mut engine());
    on_rows(app, &rows);
}

/// The running processes from one Toolhelp snapshot. Only processes not
/// seen before (a new pid, or the same pid with another name or parent) are
/// opened for their start time and path; one that can't be opened is
/// remembered and left out, not tried every second.
fn fresh_rows(e: &mut Engine) -> Vec<ProcessRow> {
    let Ok(entries) = actions::snapshot_processes() else { return Vec::new() };
    let live: HashSet<u32> = entries.iter().map(|p| p.pid).collect();
    e.light.retain(|pid, _| live.contains(pid));
    let mut rows = Vec::with_capacity(entries.len());
    for p in entries {
        if p.pid <= 4 {
            continue;
        }
        if e.light.get(&p.pid).is_none_or(|l| l.name != p.name || l.parent != p.parent) {
            let mut l = LightProc { name: p.name.clone(), parent: p.parent, start: 0, exe: None, ok: false };
            if let Ok(h) = actions::open_process(p.pid, PROCESS_QUERY_LIMITED_INFORMATION) {
                if let Some(created) = actions::handle_creation_filetime(h.0) {
                    l.start = actions::filetime_to_unix(created);
                    l.exe = actions::image_path(h.0);
                    l.ok = true;
                }
            }
            e.light.insert(p.pid, l);
        }
        let l = &e.light[&p.pid];
        if l.ok {
            rows.push(light_row(p.pid, l));
        }
    }
    rows
}

fn light_row(pid: u32, l: &LightProc) -> ProcessRow {
    ProcessRow {
        pid,
        parent_pid: None,
        name: l.name.clone(),
        exe: l.exe.clone(),
        cmd: String::new(),
        user: None,
        cpu: 0.0,
        memory: 0,
        disk_read_bps: 0.0,
        disk_write_bps: 0.0,
        gpu: None,
        status: "running".into(),
        start_time: l.start,
        kind: ProcessKind::Background,
        window_title: None,
        not_responding: false,
        suspended: false,
        session_id: None,
        net_rx_bps: None,
        net_tx_bps: None,
    }
}

/// After the rules changed: compare the running processes again right away
/// (a fresh list, so a paused sampler's old snapshot doesn't hide new apps),
/// and wake the sampler so a paused one starts its light ticks.
async fn reevaluate(app: &AppHandle) {
    let app2 = app.clone();
    let _ = blocking(move || {
        let rows = fresh_rows(&mut engine());
        on_rows(&app2, &rows);
        emit_status(&app2);
    })
    .await;
    crate::sampler::wake(app);
}

fn window_pid(hwnd: HWND) -> u32 {
    if hwnd.is_invalid() {
        return 0;
    }
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    pid
}

fn on_front(hwnd: HWND) {
    let pid = window_pid(hwnd);
    let held = crate::boost::touched();
    let changed = {
        let mut e = engine();
        e.initialized && front_changed(&mut e, pid, now_ms(), &RealOps, &held)
    };
    if changed {
        if let Some(app) = APP.get() {
            emit_status(app);
        }
    }
}

unsafe extern "system" fn win_event(_hook: HWINEVENTHOOK, _event: u32, hwnd: HWND, _object: i32, _child: i32, _thread: u32, _time: u32) {
    on_front(hwnd);
}

/// One thread with an out-of-context `EVENT_SYSTEM_FOREGROUND` hook; it
/// sleeps in `GetMessageW` and wakes only when the window in front changes.
fn start_foreground_hook() {
    let spawned = std::thread::Builder::new().name("orbit-rules-front".into()).spawn(|| unsafe {
        let hook = SetWinEventHook(EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_FOREGROUND, HMODULE::default(), Some(win_event), 0, 0, WINEVENT_OUTOFCONTEXT);
        if hook.is_invalid() {
            eprintln!("[orbit] rules: the foreground hook failed; \"only while in front\" rules use the priority for the back");
            return;
        }
        on_front(GetForegroundWindow());
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            DispatchMessageW(&msg);
        }
    });
    if let Err(err) = spawned {
        eprintln!("[orbit] rules: could not start the foreground thread: {err}");
    }
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(f).await.map_err(|e| e.to_string())
}

fn state_of(e: &Engine) -> RulesState {
    let views = e
        .rules
        .iter()
        .map(|r| {
            let stat = e.stats.get(&r.id).cloned().unwrap_or_default();
            let running = e.tracked.iter().filter(|(_, t)| t.rule_id == r.id).count() as u32;
            RuleView { rule: r.clone(), last_applied: stat.last_applied, last_failed: stat.last_failed, running }
        })
        .collect();
    RulesState {
        rules_enabled: e.enabled,
        is_admin: e.admin,
        logical_cores: e.system_mask.count_ones(),
        efficiency_supported: e.eco_supported,
        rules: views,
    }
}

#[tauri::command]
pub async fn rules_list() -> Result<RulesState, String> {
    blocking(|| state_of(&engine())).await
}

/// The refusal reason for an app, or `None` when a rule may name it.
#[tauri::command]
pub fn rules_check(value: String) -> Option<String> {
    let kind = if value.contains(['\\', '/']) { "path" } else { "name" };
    rule_refusal(kind, &value).map(str::to_string)
}

#[tauri::command]
pub async fn rules_create(app: AppHandle, rule: Rule) -> Result<RuleView, String> {
    let view = blocking(move || -> Result<RuleView, String> {
        let mut e = engine();
        if e.rules.len() >= MAX_RULES {
            return Err("That is the most rules Orbit keeps.".into());
        }
        let mut clean = validate(&rule, &e.rules, e.system_mask, e.eco_supported)?;
        clean.id = e.new_id(now_ms());
        let before = e.rules.clone();
        e.rules.push(clean.clone());
        if let Err(err) = save(&e) {
            e.rules.pop();
            e.publish();
            return Err(format!("Could not save the rule: {err}."));
        }
        e.retrack(&before);
        Ok(RuleView { rule: clean, last_applied: None, last_failed: None, running: 0 })
    })
    .await??;
    reevaluate(&app).await;
    Ok(view)
}

#[tauri::command]
pub async fn rules_update(app: AppHandle, rule: Rule) -> Result<RuleView, String> {
    let view = blocking(move || -> Result<RuleView, String> {
        let mut e = engine();
        let Some(pos) = e.rules.iter().position(|r| r.id == rule.id) else {
            return Err("That rule no longer exists.".into());
        };
        let others: Vec<Rule> = e.rules.iter().filter(|r| r.id != rule.id).cloned().collect();
        let clean = validate(&rule, &others, e.system_mask, e.eco_supported)?;
        let before = e.rules.clone();
        let old = std::mem::replace(&mut e.rules[pos], clean.clone());
        if let Err(err) = save(&e) {
            e.rules[pos] = old;
            e.publish();
            return Err(format!("Could not save the rule: {err}."));
        }
        e.retrack(&before);
        let stat = e.stats.get(&clean.id).cloned().unwrap_or_default();
        Ok(RuleView { rule: clean, last_applied: stat.last_applied, last_failed: stat.last_failed, running: 0 })
    })
    .await??;
    reevaluate(&app).await;
    Ok(view)
}

#[tauri::command]
pub async fn rules_delete(app: AppHandle, id: String) -> Result<(), String> {
    blocking(move || -> Result<(), String> {
        let mut e = engine();
        let Some(pos) = e.rules.iter().position(|r| r.id == id) else {
            return Ok(());
        };
        let before = e.rules.clone();
        let old = e.rules.remove(pos);
        if let Err(err) = save(&e) {
            e.rules.insert(pos, old);
            e.publish();
            return Err(format!("Could not save the change: {err}."));
        }
        e.stats.remove(&id);
        e.retrack(&before);
        Ok(())
    })
    .await??;
    reevaluate(&app).await;
    Ok(())
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ResetReport {
    pub reset: u32,
    pub problems: Vec<String>,
}

/// Puts the running app back to Normal priority and all cores (what the rule
/// sets) and switches the rule off.
#[tauri::command]
pub async fn rules_reset(app: AppHandle, id: String) -> Result<ResetReport, String> {
    let report = blocking(move || -> Result<ResetReport, String> {
        let mut e = engine();
        // A fresh list: the sampler's snapshot is old while updates are paused.
        let rows = fresh_rows(&mut e);
        let before = e.rules.clone();
        let (reset, problems) = reset_rule(&mut e, &id, &rows, now_ms(), &RealOps, std::process::id())?;
        if let Err(err) = save(&e) {
            e.rules = before;
            e.publish();
            return Err(format!("Put the app back, but could not switch the rule off: {err}."));
        }
        Ok(ResetReport { reset, problems })
    })
    .await??;
    emit_status(&app);
    Ok(report)
}

/// The master switch (setting `rulesEnabled`). Off: no rule is applied.
#[tauri::command]
pub async fn rules_set_enabled(app: AppHandle, enabled: bool) -> Result<RulesState, String> {
    let state = blocking(move || -> Result<RulesState, String> {
        let mut e = engine();
        let old = e.enabled;
        e.enabled = enabled;
        if let Err(err) = save(&e) {
            e.enabled = old;
            e.publish();
            return Err(format!("Could not save the change: {err}."));
        }
        let rules = e.rules.clone();
        e.retrack(&rules);
        Ok(state_of(&e))
    })
    .await??;
    reevaluate(&app).await;
    Ok(state)
}

/// The rules for Settings > Backup > Export.
#[tauri::command]
pub async fn rules_export() -> Result<RulesBackup, String> {
    blocking(|| {
        let e = engine();
        RulesBackup { rules_enabled: Some(e.enabled), rules: e.rules.clone() }
    })
    .await
}

/// Settings > Backup > Import. `apply: false` only reports what would change.
#[tauri::command]
pub async fn rules_import(app: AppHandle, backup: RulesBackup, apply: bool) -> Result<ImportReport, String> {
    let report = blocking(move || -> Result<ImportReport, String> {
        let mut e = engine();
        if backup.rules.len() > MAX_RULES * 2 {
            return Err("That file has too many rules.".into());
        }
        let now = now_ms();
        let mut next = e.next_id;
        let (merged, mut report) = merge(&e.rules, &backup.rules, e.system_mask, e.eco_supported, || {
            next += 1;
            format!("r{now:x}{next:x}")
        });
        report.switch_to = backup.rules_enabled.filter(|&on| on != e.enabled);
        if !apply {
            return Ok(report);
        }
        let (old_rules, old_enabled) = (e.rules.clone(), e.enabled);
        e.rules = merged;
        e.next_id = next;
        if let Some(on) = report.switch_to {
            e.enabled = on;
        }
        if let Err(err) = save(&e) {
            e.rules = old_rules;
            e.enabled = old_enabled;
            e.publish();
            return Err(format!("Could not save the rules: {err}."));
        }
        e.retrack(&old_rules);
        Ok(report)
    })
    .await??;
    if apply {
        reevaluate(&app).await;
    }
    Ok(report)
}

#[tauri::command]
pub async fn rules_log() -> Result<Vec<LogEntry>, String> {
    blocking(|| engine().log.iter().rev().cloned().collect()).await
}

/// Per-process status for the badge on the Processes rows.
#[tauri::command]
pub async fn rules_status() -> Result<Vec<ProcStatus>, String> {
    blocking(|| engine().statuses()).await
}

/// A file picker for an exe. `Ok(None)` when cancelled.
#[tauri::command]
pub async fn rules_pick_exe(app: AppHandle) -> Result<Option<String>, String> {
    let owner = app.get_webview_window("main").and_then(|w| w.hwnd().ok()).map(|h| h.0 as isize).unwrap_or(0);
    blocking(move || {
        std::thread::spawn(move || crate::boost::pick_exe(owner, w!("Pick an app")))
            .join()
            .unwrap_or_else(|_| Err("The file picker failed.".to_string()))
    })
    .await?
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests;
