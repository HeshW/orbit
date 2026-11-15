//! Per-app rules (v0.9, 11a), Optimize > Rules.
//!
//! A rule says "whenever this app runs, give it this CPU priority (and these
//! cores)". It matches by full exe path or by exe name, sets a priority
//! (Low to High, never Realtime) and/or an affinity mask, and Orbit applies it
//! to every matching process.
//!
//! No thread, watcher or extra query of its own: the sampler calls
//! `on_snapshot` after every tick, and a process that appears there (new pid
//! + start time, like Boost's detection) is checked against the rules and
//! changed within that same tick. Processes that have a rule are then
//! re-checked each tick with one cheap read (`GetPriorityClass` / the affinity
//! mask). If something changed the value back, Orbit sets it again; if that
//! happens 3 times within 60 seconds it stops for that process and says so,
//! instead of fighting in a loop.
//!
//! Safety:
//! - Every change goes through `actions::set_priority` / `actions::set_affinity`
//!   (so `guard_pid` and the critical-process guard apply). On top of that a
//!   rule can never name Orbit, Windows' own processes, Windows Security
//!   (Defender) or anti-cheat services, and rows grouped as Windows are never
//!   touched.
//! - Realtime is rejected, and so is an affinity that leaves no core.
//! - A failure is recorded once per process instance with a plain reason and
//!   is not retried.
//!
//! Rules live in `rules.json` under `<app local data>\rules`, or under
//! `ORBIT_RULES_DIR` in debug builds. The log and the per-process status are
//! in memory only.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager};
use windows::core::w;
use windows::Win32::System::Threading::{
    GetCurrentProcess, GetPriorityClass, PROCESS_QUERY_LIMITED_INFORMATION,
};

use crate::actions::{self, ACCESS_DENIED, EXITED};
use crate::model::{ProcessKind, ProcessRow, Snapshot};
use crate::sampler::AppState;

/// Sent when a process's rule status changes (applied, failed, stopped, gone).
pub const RULES_EVENT: &str = "orbit://rules";
const FILE_NAME: &str = "rules.json";
const MAX_RULES: usize = 200;
const MAX_LOG: usize = 200;
/// Reverts within this window make Orbit stop for that process.
const REVERT_WINDOW_MS: u64 = 60_000;
const REVERT_LIMIT: usize = 3;

pub const PRIORITIES: [&str; 5] = ["low", "belowNormal", "normal", "aboveNormal", "high"];

pub const NEEDS_ADMIN: &str = "This app runs as administrator. Restart Orbit as administrator to change it.";
pub const BLOCKED: &str = "Windows or the app's protection blocked this change.";
pub const CHANGED_BACK: &str = "Kept being changed back by something else.";

const WHY_ORBIT: &str = "Orbit can't change itself.";
const WHY_WINDOWS: &str = "Windows needs this process. Orbit won't change it.";
const WHY_DEFENDER: &str = "Windows Security protects itself. Orbit won't change it.";
const WHY_ANTICHEAT: &str = "Anti-cheat software blocks changes like this. Orbit won't try.";

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
    pub enabled: bool,
}

impl Default for Rule {
    fn default() -> Self {
        Self {
            id: String::new(),
            matcher: RuleMatch { kind: "name".into(), value: String::new() },
            priority: None,
            affinity: None,
            enabled: true,
        }
    }
}

impl Rule {
    fn mask(&self) -> Option<u64> {
        self.affinity.as_deref().and_then(|m| m.trim().parse().ok())
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
    pub rules: Vec<RuleView>,
}

/// "applied" | "blocked" | "reverted".
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

// ---------------------------------------------------------------------------
// Which names a rule may never target
// ---------------------------------------------------------------------------

const WINDOWS_NAMES: &[&str] = &[
    "csrss.exe", "wininit.exe", "smss.exe", "lsass.exe", "lsaiso.exe", "services.exe",
    "winlogon.exe", "svchost.exe", "dwm.exe", "fontdrvhost.exe", "system",
    "system idle process", "registry", "secure system", "memory compression",
];
const DEFENDER_NAMES: &[&str] = &["msmpeng.exe", "nissrv.exe", "mpdefendercoreservice.exe"];
const ANTICHEAT_NAMES: &[&str] = &["vgc.exe", "vgtray.exe"];
const ANTICHEAT_PREFIXES: &[&str] =
    &["vgk", "easyanticheat", "beservice", "bedaisy", "equ8", "xigncode", "gameguard", "faceit"];

/// File name part of a Windows path or a bare name.
fn file_name(s: &str) -> &str {
    s.trim().trim_matches('"').rsplit(['\\', '/']).next().unwrap_or("").trim()
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
    if ANTICHEAT_NAMES.contains(&name.as_str()) || ANTICHEAT_PREFIXES.iter().any(|p| name.starts_with(p)) {
        return Some(WHY_ANTICHEAT);
    }
    if value.contains(['\\', '/']) {
        let path = actions::normalize_path(value);
        let root = std::env::var("SystemRoot").or_else(|_| std::env::var("windir")).unwrap_or_else(|_| r"C:\Windows".into());
        let root = format!("{}\\", actions::normalize_path(&root).trim_end_matches('\\'));
        if path.starts_with(&root) {
            return Some(WHY_WINDOWS);
        }
    }
    None
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

/// The rule for a process: a path rule beats a name rule; otherwise the first.
fn find_rule<'a>(rules: &'a [Rule], row: &ProcessRow) -> Option<&'a Rule> {
    let hit = |r: &&Rule| r.enabled && rule_matches(r, &row.name, row.exe.as_deref());
    rules.iter().filter(|r| r.matcher.kind == "path").find(hit).or_else(|| rules.iter().filter(|r| r.matcher.kind == "name").find(hit))
}

/// Checks a rule from the UI and returns the cleaned version. `others` are the
/// existing rules (the one being edited excluded) for the duplicate check.
pub fn validate(input: &Rule, others: &[Rule], system_mask: u64) -> Result<Rule, String> {
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
    if let Some(why) = refusal(&value) {
        return Err(why.into());
    }
    let priority = match input.priority.as_deref().map(str::trim).filter(|p| !p.is_empty()) {
        None => None,
        Some("realtime") => return Err("Orbit never sets Realtime. It can freeze Windows.".into()),
        Some(p) if PRIORITIES.contains(&p) => Some(p.to_string()),
        Some(_) => return Err("That priority is not valid.".into()),
    };
    let affinity = match input.affinity.as_deref().map(str::trim).filter(|m| !m.is_empty()) {
        None => None,
        Some(m) => Some(actions::validate_mask(m, system_mask)?.to_string()),
    };
    if priority.is_none() && affinity.is_none() {
        return Err("Pick a priority or some processors to set.".into());
    }
    let rule = Rule {
        id: input.id.clone(),
        matcher: RuleMatch { kind: kind.to_string(), value },
        priority,
        affinity,
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

/// "High priority, 4 cores".
pub fn summary(rule: &Rule) -> String {
    let mut parts = Vec::new();
    if let Some(p) = &rule.priority {
        parts.push(format!("{} priority", priority_label(p)));
    }
    if let Some(m) = rule.mask() {
        let n = m.count_ones();
        parts.push(format!("{n} {}", if n == 1 { "core" } else { "cores" }));
    }
    parts.join(", ")
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
    state: State,
    summary: String,
    reason: Option<String>,
    needs_admin: bool,
    at: u64,
    reverts: Vec<u64>,
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
    next_id: u64,
}

impl Engine {
    pub fn new(admin: bool, system_mask: u64) -> Self {
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
            next_id: 0,
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

    /// Forgets what was learned about processes, so the next tick compares
    /// every running process with the rules again.
    fn reset_tracking(&mut self) -> bool {
        self.checked.clear();
        let had = !self.tracked.is_empty();
        self.tracked.clear();
        had
    }

    fn new_id(&mut self, now: u64) -> String {
        self.next_id += 1;
        format!("r{:x}{:x}", now, self.next_id)
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
    fn guard(&self, pid: u32) -> Result<(), String>;
    /// "low" | "belowNormal" | "normal" | "aboveNormal" | "high" | "realtime".
    fn priority(&self, pid: u32, start: u64) -> Result<String, String>;
    fn set_priority(&self, pid: u32, start: u64, name: &str) -> Result<(), String>;
    fn affinity(&self, pid: u32, start: u64) -> Result<u64, String>;
    fn set_affinity(&self, pid: u32, start: u64, mask: u64) -> Result<(), String>;
}

/// Turns an error from `actions` into the text the owner sees. `None` means the
/// process is gone and nothing should be said.
fn explain(err: &str, admin: bool) -> Option<(String, bool)> {
    if err == EXITED {
        return None;
    }
    if err == ACCESS_DENIED {
        return Some(if admin { (BLOCKED.to_string(), false) } else { (NEEDS_ADMIN.to_string(), true) });
    }
    Some((err.to_string(), false))
}

/// Compares a process with its rule and fixes what differs. Returns whether
/// anything was changed, or the error.
fn apply_rule(rule: &Rule, row: &ProcessRow, ops: &dyn Ops) -> Result<bool, String> {
    ops.guard(row.pid)?;
    let mut changed = false;
    if let Some(p) = &rule.priority {
        if ops.priority(row.pid, row.start_time)? != *p {
            ops.set_priority(row.pid, row.start_time, p)?;
            changed = true;
        }
    }
    if let Some(m) = rule.mask() {
        if ops.affinity(row.pid, row.start_time)? != m {
            ops.set_affinity(row.pid, row.start_time, m)?;
            changed = true;
        }
    }
    Ok(changed)
}

fn app_log(kind: &str, rule_id: &str, row_name: &str, pid: u32, now: u64, text: String, needs_admin: bool) -> LogEntry {
    LogEntry { at: now, kind: kind.into(), rule_id: rule_id.to_string(), app: row_name.to_string(), pid, text, needs_admin }
}

/// One tick: new processes get their rule, tracked ones are re-checked.
/// Returns true when a status changed (so the UI should hear about it).
pub fn step(e: &mut Engine, rows: &[ProcessRow], now: u64, ops: &dyn Ops, own_pid: u32) -> bool {
    if !e.enabled || !e.rules.iter().any(|r| r.enabled) {
        e.checked.clear();
        let had = !e.tracked.is_empty();
        e.tracked.clear();
        return had;
    }
    let mut changed = false;

    // Processes that ended.
    let before = e.tracked.len();
    e.tracked.retain(|k, _| rows.iter().any(|r| r.pid == k.0 && r.start_time == k.1));
    changed |= e.tracked.len() != before;
    if e.checked.len() > rows.len() + 64 {
        let live: HashSet<Key> = rows.iter().map(|r| (r.pid, r.start_time)).collect();
        e.checked.retain(|k| live.contains(k));
    }

    // New processes.
    for row in rows {
        let key = (row.pid, row.start_time);
        if e.tracked.contains_key(&key) || e.checked.contains(&key) {
            continue;
        }
        if row.pid <= 4 || row.pid == own_pid || row.kind == ProcessKind::Windows || refusal(&row.name).is_some() {
            e.checked.insert(key);
            continue;
        }
        let Some(rule) = find_rule(&e.rules, row).cloned() else {
            e.checked.insert(key);
            continue;
        };
        let sum = summary(&rule);
        match apply_rule(&rule, row, ops) {
            Ok(did) => {
                let text = if did { format!("Set to {sum}.") } else { format!("Already set to {sum}.") };
                e.push_log(app_log("applied", &rule.id, &row.name, row.pid, now, text, false));
                e.applied(&rule.id, now);
                e.tracked.insert(key, Tracked { rule_id: rule.id, state: State::Applied, summary: sum, reason: None, needs_admin: false, at: now, reverts: Vec::new() });
                changed = true;
            }
            Err(err) => match explain(&err, e.admin) {
                None => {
                    e.checked.insert(key);
                }
                Some((reason, needs_admin)) => {
                    e.push_log(app_log("blocked", &rule.id, &row.name, row.pid, now, reason.clone(), needs_admin));
                    e.failed(&rule.id, now, &reason, needs_admin);
                    e.tracked.insert(key, Tracked { rule_id: rule.id, state: State::Failed, summary: sum, reason: Some(reason), needs_admin, at: now, reverts: Vec::new() });
                    changed = true;
                }
            },
        }
    }

    // Re-check the ones that hold a rule.
    let keys: Vec<Key> = e.tracked.iter().filter(|(_, t)| t.state == State::Applied).map(|(k, _)| *k).collect();
    for key in keys {
        let Some(row) = rows.iter().find(|r| r.pid == key.0 && r.start_time == key.1) else { continue };
        let Some(rule) = e.tracked.get(&key).and_then(|t| e.rules.iter().find(|r| r.id == t.rule_id)).cloned() else {
            e.tracked.remove(&key);
            changed = true;
            continue;
        };
        // What drifted. A failed read is skipped: it worked when the rule was applied.
        let mut drift: Vec<String> = Vec::new();
        let mut gone = false;
        if let Some(p) = &rule.priority {
            match ops.priority(key.0, key.1) {
                Ok(cur) if cur != *p => drift.push(format!("The priority was changed to {}.", priority_label(&cur))),
                Err(err) if err == EXITED => gone = true,
                _ => {}
            }
        }
        if let Some(m) = rule.mask() {
            match ops.affinity(key.0, key.1) {
                Ok(cur) if cur != m => drift.push("The processors were changed.".to_string()),
                Err(err) if err == EXITED => gone = true,
                _ => {}
            }
        }
        if gone {
            e.tracked.remove(&key);
            changed = true;
            continue;
        }
        if drift.is_empty() {
            continue;
        }
        let sum = summary(&rule);
        let verdict = {
            let t = e.tracked.get_mut(&key).expect("tracked");
            note_revert(&mut t.reverts, now)
        };
        changed = true;
        if verdict == Revert::GiveUp {
            let t = e.tracked.get_mut(&key).expect("tracked");
            t.state = State::GaveUp;
            t.reason = Some(CHANGED_BACK.to_string());
            t.at = now;
            e.push_log(app_log("reverted", &rule.id, &row.name, row.pid, now, format!("{CHANGED_BACK} Orbit stopped for this run."), false));
            e.failed(&rule.id, now, CHANGED_BACK, false);
            continue;
        }
        match apply_rule(&rule, row, ops) {
            Ok(_) => {
                e.push_log(app_log("reverted", &rule.id, &row.name, row.pid, now, format!("{} Orbit set {sum} again.", drift.join(" ")), false));
                e.applied(&rule.id, now);
                if let Some(t) = e.tracked.get_mut(&key) {
                    t.at = now;
                }
            }
            Err(err) => match explain(&err, e.admin) {
                None => {
                    e.tracked.remove(&key);
                }
                Some((reason, needs_admin)) => {
                    e.push_log(app_log("blocked", &rule.id, &row.name, row.pid, now, reason.clone(), needs_admin));
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

// ---------------------------------------------------------------------------
// Real OS calls
// ---------------------------------------------------------------------------

pub struct RealOps;

impl RealOps {
    fn open(pid: u32, start: u64) -> Result<actions::OwnedHandle, String> {
        let h = actions::open_process(pid, PROCESS_QUERY_LIMITED_INFORMATION)?;
        actions::verify_identity(h.0, Some(start))?;
        Ok(h)
    }
}

impl Ops for RealOps {
    fn guard(&self, pid: u32) -> Result<(), String> {
        actions::guard_pid(pid, "changed")
    }

    fn priority(&self, pid: u32, start: u64) -> Result<String, String> {
        let h = Self::open(pid, start)?;
        let class = unsafe { GetPriorityClass(h.0) };
        if class == 0 {
            let err = windows::core::Error::from_win32();
            return Err(actions::describe(&err, "read the priority"));
        }
        Ok(match actions::priority_name(class) {
            Some("idle") => "low".to_string(),
            Some(name) => name.to_string(),
            None => "unknown".to_string(),
        })
    }

    fn set_priority(&self, pid: u32, start: u64, name: &str) -> Result<(), String> {
        let name = if name == "low" { "idle" } else { name };
        if name == "realtime" {
            return Err("Orbit never sets Realtime.".into());
        }
        actions::set_priority(pid, name.to_string(), Some(start))
    }

    fn affinity(&self, pid: u32, start: u64) -> Result<u64, String> {
        let h = Self::open(pid, start)?;
        actions::read_affinity(h.0).map(|(process, _)| process)
    }

    fn set_affinity(&self, pid: u32, start: u64, mask: u64) -> Result<(), String> {
        actions::set_affinity(pid, mask.to_string(), Some(start))
    }
}

// ---------------------------------------------------------------------------
// Files
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Default)]
#[serde(default)]
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

/// Reads the rules file, dropping anything that is no longer valid.
fn load(dir: &Path, system_mask: u64) -> (bool, Vec<Rule>) {
    let stored: Stored = fs::read(dir.join(FILE_NAME)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
    let mut rules: Vec<Rule> = Vec::new();
    for r in &stored.rules {
        if r.id.is_empty() {
            continue;
        }
        if let Ok(clean) = validate(r, &rules, system_mask) {
            rules.push(clean);
        }
        if rules.len() == MAX_RULES {
            break;
        }
    }
    (stored.rules_enabled.unwrap_or(true), rules)
}

fn save(e: &Engine) -> Result<(), String> {
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
            Mutex::new(Engine::new(admin, system))
        })
        .lock()
        .unwrap_or_else(|p| p.into_inner())
}

/// Setup hook: loads the saved rules.
pub fn init(app: &AppHandle) {
    let dir = rules_dir(app.path().app_local_data_dir().ok());
    let mut e = engine();
    if let Some(d) = dir.as_deref() {
        let (enabled, rules) = load(d, e.system_mask);
        e.enabled = enabled;
        e.rules = rules;
    }
    e.dir = dir;
    e.initialized = true;
}

fn emit_status(app: &AppHandle) {
    let statuses = engine().statuses();
    let _ = app.emit(RULES_EVENT, RulesEvent { statuses });
}

/// Called by the sampler after every snapshot (no thread of its own).
pub fn on_snapshot(app: &AppHandle, snapshot: &Snapshot) {
    let changed = {
        let mut e = engine();
        if !e.initialized {
            false
        } else {
            step(&mut e, &snapshot.processes, now_ms(), &RealOps, std::process::id())
        }
    };
    if changed {
        emit_status(app);
    }
}

/// After the rules changed: compare the running processes again right away.
fn reevaluate(app: &AppHandle) {
    if let Some(snap) = app.state::<AppState>().latest_snapshot() {
        on_snapshot(app, &snap);
    }
    emit_status(app);
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
    RulesState { rules_enabled: e.enabled, is_admin: e.admin, logical_cores: e.system_mask.count_ones(), rules: views }
}

#[tauri::command]
pub async fn rules_list() -> Result<RulesState, String> {
    blocking(|| state_of(&engine())).await
}

/// The refusal reason for an app, or `None` when a rule may name it.
#[tauri::command]
pub fn rules_check(value: String) -> Option<String> {
    refusal(&value).map(str::to_string)
}

#[tauri::command]
pub async fn rules_create(app: AppHandle, rule: Rule) -> Result<RuleView, String> {
    let view = blocking(move || -> Result<RuleView, String> {
        let mut e = engine();
        if e.rules.len() >= MAX_RULES {
            return Err("That is the most rules Orbit keeps.".into());
        }
        let mut clean = validate(&rule, &e.rules, e.system_mask)?;
        clean.id = e.new_id(now_ms());
        e.rules.push(clean.clone());
        if let Err(err) = save(&e) {
            e.rules.pop();
            return Err(format!("Could not save the rule: {err}."));
        }
        e.reset_tracking();
        Ok(RuleView { rule: clean, last_applied: None, last_failed: None, running: 0 })
    })
    .await??;
    reevaluate(&app);
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
        let clean = validate(&rule, &others, e.system_mask)?;
        let old = std::mem::replace(&mut e.rules[pos], clean.clone());
        if let Err(err) = save(&e) {
            e.rules[pos] = old;
            return Err(format!("Could not save the rule: {err}."));
        }
        e.reset_tracking();
        let stat = e.stats.get(&clean.id).cloned().unwrap_or_default();
        Ok(RuleView { rule: clean, last_applied: stat.last_applied, last_failed: stat.last_failed, running: 0 })
    })
    .await??;
    reevaluate(&app);
    Ok(view)
}

#[tauri::command]
pub async fn rules_delete(app: AppHandle, id: String) -> Result<(), String> {
    blocking(move || -> Result<(), String> {
        let mut e = engine();
        let Some(pos) = e.rules.iter().position(|r| r.id == id) else {
            return Ok(());
        };
        let old = e.rules.remove(pos);
        if let Err(err) = save(&e) {
            e.rules.insert(pos, old);
            return Err(format!("Could not save the change: {err}."));
        }
        e.stats.remove(&id);
        e.reset_tracking();
        Ok(())
    })
    .await??;
    reevaluate(&app);
    Ok(())
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
            return Err(format!("Could not save the change: {err}."));
        }
        e.reset_tracking();
        Ok(state_of(&e))
    })
    .await??;
    reevaluate(&app);
    Ok(state)
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
mod tests {
    use super::*;
    use std::cell::RefCell;

    const ME: u32 = 99;

    fn row(pid: u32, name: &str, exe: Option<&str>) -> ProcessRow {
        ProcessRow {
            pid,
            parent_pid: None,
            name: name.to_string(),
            exe: exe.map(str::to_string),
            cmd: String::new(),
            user: None,
            cpu: 0.0,
            memory: 0,
            disk_read_bps: 0.0,
            disk_write_bps: 0.0,
            gpu: None,
            status: "running".into(),
            start_time: 1000 + pid as u64,
            kind: ProcessKind::Background,
            window_title: None,
            not_responding: false,
            suspended: false,
            session_id: Some(1),
            net_rx_bps: None,
            net_tx_bps: None,
        }
    }

    fn rule(id: &str, kind: &str, value: &str, priority: Option<&str>, mask: Option<&str>) -> Rule {
        Rule {
            id: id.into(),
            matcher: RuleMatch { kind: kind.into(), value: value.into() },
            priority: priority.map(str::to_string),
            affinity: mask.map(str::to_string),
            enabled: true,
        }
    }

    #[derive(Default)]
    struct Fake {
        prio: RefCell<HashMap<u32, String>>,
        mask: RefCell<HashMap<u32, u64>>,
        fail: RefCell<Option<String>>,
        sets: RefCell<u32>,
    }

    impl Ops for Fake {
        fn guard(&self, _: u32) -> Result<(), String> {
            Ok(())
        }
        fn priority(&self, pid: u32, _: u64) -> Result<String, String> {
            self.prio.borrow().get(&pid).cloned().ok_or_else(|| EXITED.to_string())
        }
        fn set_priority(&self, pid: u32, _: u64, name: &str) -> Result<(), String> {
            if let Some(e) = self.fail.borrow().clone() {
                return Err(e);
            }
            *self.sets.borrow_mut() += 1;
            self.prio.borrow_mut().insert(pid, name.to_string());
            Ok(())
        }
        fn affinity(&self, pid: u32, _: u64) -> Result<u64, String> {
            self.mask.borrow().get(&pid).copied().ok_or_else(|| EXITED.to_string())
        }
        fn set_affinity(&self, pid: u32, _: u64, mask: u64) -> Result<(), String> {
            *self.sets.borrow_mut() += 1;
            self.mask.borrow_mut().insert(pid, mask);
            Ok(())
        }
    }

    fn engine_with(rules: Vec<Rule>, admin: bool) -> Engine {
        let mut e = Engine::new(admin, 0b1111);
        e.rules = rules;
        e.initialized = true;
        e
    }

    // ---- matching

    #[test]
    fn name_matches_ignore_case_and_folder() {
        let r = rule("a", "name", "League of Legends.exe", Some("high"), None);
        assert!(rule_matches(&r, "league of legends.EXE", Some(r"D:\Games\Riot\League of Legends.exe")));
        assert!(rule_matches(&r, "League of Legends.exe", None));
        assert!(!rule_matches(&r, "league.exe", None));
    }

    #[test]
    fn path_matches_normalise_slashes_case_and_prefix() {
        let r = rule("a", "path", r"D:\Games\Riot\LoL.exe", Some("high"), None);
        assert!(rule_matches(&r, "LoL.exe", Some(r"d:/games/riot/lol.EXE")));
        assert!(rule_matches(&r, "LoL.exe", Some(r"\\?\D:\Games\Riot\LoL.exe")));
        assert!(!rule_matches(&r, "LoL.exe", Some(r"E:\Games\Riot\LoL.exe")));
        // A path rule never matches a process whose path is hidden.
        assert!(!rule_matches(&r, "LoL.exe", None));
    }

    #[test]
    fn a_path_rule_beats_a_name_rule() {
        let rules = vec![
            rule("n", "name", "game.exe", Some("low"), None),
            rule("p", "path", r"C:\Games\game.exe", Some("high"), None),
        ];
        let r = row(5, "game.exe", Some(r"C:\Games\game.exe"));
        assert_eq!(find_rule(&rules, &r).unwrap().id, "p");
        let other = row(6, "game.exe", Some(r"D:\game.exe"));
        assert_eq!(find_rule(&rules, &other).unwrap().id, "n");
    }

    #[test]
    fn disabled_rules_do_not_match() {
        let mut r = rule("n", "name", "game.exe", Some("low"), None);
        r.enabled = false;
        assert!(find_rule(&[r], &row(5, "game.exe", None)).is_none());
    }

    // ---- validation

    #[test]
    fn validation_cleans_and_rejects() {
        let ok = validate(&rule("", "name", "  game.exe ", Some("high"), Some("3")), &[], 0b1111).unwrap();
        assert_eq!(ok.matcher.value, "game.exe");
        assert_eq!(ok.affinity.as_deref(), Some("3"));
        let err = |r: Rule| validate(&r, &[], 0b1111).unwrap_err();
        assert!(err(rule("", "name", "game.exe", Some("realtime"), None)).contains("Realtime"));
        assert!(err(rule("", "name", "game.exe", Some("turbo"), None)).contains("not valid"));
        assert!(err(rule("", "name", "game.exe", None, None)).contains("Pick a priority"));
        assert!(err(rule("", "name", "", Some("high"), None)).contains("Pick an app"));
        assert!(err(rule("", "name", "game.txt", Some("high"), None)).contains(".exe"));
        assert!(err(rule("", "name", r"C:\game.exe", Some("high"), None)).contains("file name"));
        assert!(err(rule("", "path", "game.exe", Some("high"), None)).contains("full path"));
        assert!(err(rule("", "folder", "game.exe", Some("high"), None)).contains("path or a name"));
    }

    #[test]
    fn affinity_must_fit_the_machine_and_keep_a_core() {
        let err = |m: &str| validate(&rule("", "name", "g.exe", None, Some(m)), &[], 0b1111).unwrap_err();
        assert_eq!(err("0"), "Pick at least one processor.");
        assert!(err("16").contains("processors this PC does not have"));
        assert!(err("abc").contains("not valid"));
        assert!(validate(&rule("", "name", "g.exe", None, Some("15")), &[], 0b1111).is_ok());
    }

    #[test]
    fn duplicates_are_refused_but_editing_yourself_is_fine() {
        let a = rule("a", "name", "Game.exe", Some("high"), None);
        let dup = rule("", "name", "game.EXE", Some("low"), None);
        assert!(validate(&dup, &[a.clone()], 15).unwrap_err().contains("already a rule"));
        let edit = rule("a", "name", "game.exe", Some("low"), None);
        assert!(validate(&edit, &[a], 15).is_ok());
    }

    // ---- guard list

    #[test]
    fn the_guard_list_refuses_system_security_and_anti_cheat() {
        for n in ["orbit.exe", "ORBIT.EXE", r"C:\x\Orbit.exe"] {
            assert_eq!(refusal(n), Some(WHY_ORBIT), "{n}");
        }
        for n in ["csrss.exe", "lsass.exe", "smss.exe", "wininit.exe", "services.exe", "winlogon.exe", "svchost.exe", "System"] {
            assert_eq!(refusal(n), Some(WHY_WINDOWS), "{n}");
        }
        for n in ["MsMpEng.exe", "NisSrv.exe", "MpDefenderCoreService.exe"] {
            assert_eq!(refusal(n), Some(WHY_DEFENDER), "{n}");
        }
        for n in ["vgc.exe", "vgk.sys", "EasyAntiCheat.exe", "EasyAntiCheat_EOS.exe", "BEService.exe", "BEService_x64.exe", "BEDaisy.sys"] {
            assert_eq!(refusal(n), Some(WHY_ANTICHEAT), "{n}");
        }
        assert_eq!(refusal(r"C:\Windows\System32\notepad.exe"), Some(WHY_WINDOWS));
        assert_eq!(refusal("League of Legends.exe"), None);
        assert_eq!(refusal(r"D:\Games\game.exe"), None);
        // Refused by validate() too, with the plain reason.
        assert_eq!(validate(&rule("", "name", "csrss.exe", Some("high"), None), &[], 15).unwrap_err(), WHY_WINDOWS);
        assert_eq!(validate(&rule("", "name", "vgc.exe", Some("high"), None), &[], 15).unwrap_err(), WHY_ANTICHEAT);
    }

    // ---- revert counter

    #[test]
    fn three_reverts_inside_a_minute_give_up() {
        let mut r = Vec::new();
        assert_eq!(note_revert(&mut r, 1_000), Revert::Reapply);
        assert_eq!(note_revert(&mut r, 20_000), Revert::Reapply);
        assert_eq!(note_revert(&mut r, 59_999), Revert::GiveUp);
    }

    #[test]
    fn old_reverts_fall_out_of_the_window() {
        let mut r = Vec::new();
        assert_eq!(note_revert(&mut r, 0), Revert::Reapply);
        assert_eq!(note_revert(&mut r, 30_000), Revert::Reapply);
        // The first is 61 s old by now.
        assert_eq!(note_revert(&mut r, 61_000), Revert::Reapply);
        assert_eq!(r.len(), 2);
    }

    // ---- engine with fake OS calls

    #[test]
    fn a_new_matching_process_is_set_in_the_same_tick() {
        let mut e = engine_with(vec![rule("a", "name", "game.exe", Some("high"), Some("3"))], true);
        let ops = Fake::default();
        ops.prio.borrow_mut().insert(10, "low".into());
        ops.mask.borrow_mut().insert(10, 15);
        let rows = vec![row(10, "game.exe", Some(r"C:\g\game.exe")), row(11, "other.exe", None)];
        assert!(step(&mut e, &rows, 5_000, &ops, ME));
        assert_eq!(ops.prio.borrow()[&10], "high");
        assert_eq!(ops.mask.borrow()[&10], 3);
        let st = e.statuses();
        assert_eq!(st.len(), 1);
        assert_eq!((st[0].pid, st[0].state.as_str(), st[0].summary.as_str()), (10, "applied", "High priority, 2 cores"));
        assert_eq!(e.log.back().unwrap().kind, "applied");
        assert_eq!(e.stats["a"].last_applied, Some(5_000));
        // Nothing to do next tick.
        assert!(!step(&mut e, &rows, 6_000, &ops, ME));
        assert_eq!(*ops.sets.borrow(), 2);
    }

    #[test]
    fn an_app_already_at_the_value_is_logged_not_touched() {
        let mut e = engine_with(vec![rule("a", "name", "game.exe", Some("high"), None)], true);
        let ops = Fake::default();
        ops.prio.borrow_mut().insert(10, "high".into());
        step(&mut e, &[row(10, "game.exe", None)], 1, &ops, ME);
        assert_eq!(*ops.sets.borrow(), 0);
        assert!(e.log.back().unwrap().text.starts_with("Already set"));
    }

    #[test]
    fn reapplies_twice_then_stops_on_the_third_revert() {
        let mut e = engine_with(vec![rule("a", "name", "game.exe", Some("high"), None)], true);
        let ops = Fake::default();
        ops.prio.borrow_mut().insert(10, "low".into());
        let rows = vec![row(10, "game.exe", None)];
        step(&mut e, &rows, 0, &ops, ME);
        for (i, t) in [10_000u64, 20_000].into_iter().enumerate() {
            ops.prio.borrow_mut().insert(10, "normal".into());
            assert!(step(&mut e, &rows, t, &ops, ME), "revert {i}");
            assert_eq!(ops.prio.borrow()[&10], "high");
            assert_eq!(e.statuses()[0].state, "applied");
        }
        ops.prio.borrow_mut().insert(10, "normal".into());
        assert!(step(&mut e, &rows, 30_000, &ops, ME));
        assert_eq!(ops.prio.borrow()[&10], "normal", "not set a third time");
        let s = &e.statuses()[0];
        assert_eq!((s.state.as_str(), s.reason.as_deref()), ("gaveUp", Some(CHANGED_BACK)));
        assert_eq!(e.stats["a"].last_failed.as_ref().unwrap().reason, CHANGED_BACK);
        // Stays stopped, and checks nothing more for this run.
        ops.prio.borrow_mut().insert(10, "low".into());
        assert!(!step(&mut e, &rows, 40_000, &ops, ME));
        assert_eq!(ops.prio.borrow()[&10], "low");
        // A new run of the app (new start time) gets the rule again.
        let mut again = row(10, "game.exe", None);
        again.start_time += 500;
        step(&mut e, &[again], 50_000, &ops, ME);
        assert_eq!(ops.prio.borrow()[&10], "high");
    }

    #[test]
    fn an_affinity_that_changes_back_is_set_again() {
        let mut e = engine_with(vec![rule("a", "name", "game.exe", None, Some("3"))], true);
        let ops = Fake::default();
        ops.mask.borrow_mut().insert(10, 15);
        let rows = vec![row(10, "game.exe", None)];
        step(&mut e, &rows, 0, &ops, ME);
        ops.mask.borrow_mut().insert(10, 15);
        step(&mut e, &rows, 5_000, &ops, ME);
        assert_eq!(ops.mask.borrow()[&10], 3);
        assert!(e.log.back().unwrap().text.contains("processors"));
    }

    #[test]
    fn access_denied_says_why_once_and_does_not_retry() {
        for (admin, want, flag) in [(false, NEEDS_ADMIN, true), (true, BLOCKED, false)] {
            let mut e = engine_with(vec![rule("a", "name", "game.exe", Some("high"), None)], admin);
            let ops = Fake::default();
            ops.prio.borrow_mut().insert(10, "normal".into());
            *ops.fail.borrow_mut() = Some(ACCESS_DENIED.to_string());
            let rows = vec![row(10, "game.exe", None)];
            assert!(step(&mut e, &rows, 1, &ops, ME));
            let s = &e.statuses()[0];
            assert_eq!((s.state.as_str(), s.reason.as_deref(), s.needs_admin), ("failed", Some(want), flag));
            let entry = e.log.back().unwrap();
            assert_eq!((entry.kind.as_str(), entry.needs_admin), ("blocked", flag));
            assert_eq!(e.stats["a"].last_failed.as_ref().unwrap().reason, want);
            // No retry on later ticks, no second log line.
            *ops.fail.borrow_mut() = None;
            assert!(!step(&mut e, &rows, 2_000, &ops, ME));
            assert_eq!(e.log.len(), 1);
            assert_eq!(*ops.sets.borrow(), 0);
        }
    }

    #[test]
    fn exited_processes_are_forgotten_quietly() {
        let mut e = engine_with(vec![rule("a", "name", "game.exe", Some("high"), None)], true);
        let ops = Fake::default();
        ops.prio.borrow_mut().insert(10, "low".into());
        step(&mut e, &[row(10, "game.exe", None)], 0, &ops, ME);
        assert_eq!(e.tracked.len(), 1);
        assert!(step(&mut e, &[], 1_000, &ops, ME));
        assert!(e.tracked.is_empty());
        // The pid vanished from the OS before the new-process apply: no log line.
        let mut e = engine_with(vec![rule("a", "name", "game.exe", Some("high"), None)], true);
        step(&mut e, &[row(77, "game.exe", None)], 0, &Fake::default(), ME);
        assert!(e.log.is_empty() && e.tracked.is_empty());
    }

    #[test]
    fn never_touches_orbit_windows_rows_or_refused_names() {
        let mut e = engine_with(vec![
            rule("a", "name", "game.exe", Some("high"), None),
            rule("b", "name", "csrss.exe", Some("high"), None),
            rule("c", "name", "vgc.exe", Some("high"), None),
        ], true);
        let ops = Fake::default();
        let mut win = row(20, "game.exe", None);
        win.kind = ProcessKind::Windows;
        let rows = vec![row(ME, "game.exe", None), win, row(21, "csrss.exe", None), row(22, "vgc.exe", None), row(3, "game.exe", None)];
        for r in &rows {
            ops.prio.borrow_mut().insert(r.pid, "normal".into());
        }
        step(&mut e, &rows, 0, &ops, ME);
        assert_eq!(*ops.sets.borrow(), 0);
        assert!(e.statuses().is_empty());
    }

    #[test]
    fn the_master_switch_and_disabled_rules_stop_everything() {
        let mut e = engine_with(vec![rule("a", "name", "game.exe", Some("high"), None)], true);
        let ops = Fake::default();
        ops.prio.borrow_mut().insert(10, "low".into());
        let rows = vec![row(10, "game.exe", None)];
        step(&mut e, &rows, 0, &ops, ME);
        e.enabled = false;
        assert!(step(&mut e, &rows, 1_000, &ops, ME), "statuses cleared");
        assert!(e.statuses().is_empty());
        ops.prio.borrow_mut().insert(10, "low".into());
        step(&mut e, &rows, 2_000, &ops, ME);
        assert_eq!(ops.prio.borrow()[&10], "low");
        e.enabled = true;
        e.reset_tracking();
        step(&mut e, &rows, 3_000, &ops, ME);
        assert_eq!(ops.prio.borrow()[&10], "high");
    }

    #[test]
    fn the_log_keeps_the_last_200() {
        let mut e = engine_with(vec![], true);
        for i in 0..250 {
            e.push_log(app_log("applied", "a", "x.exe", i, i as u64, String::new(), false));
        }
        assert_eq!(e.log.len(), MAX_LOG);
        assert_eq!(e.log.front().unwrap().pid, 50);
    }

    // ---- files

    #[test]
    fn rules_round_trip_through_the_file_and_bad_ones_are_dropped() {
        let dir = std::env::temp_dir().join(format!("orbit-rules-file-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let mut e = engine_with(vec![rule("a", "name", "game.exe", Some("high"), Some("3"))], true);
        e.dir = Some(dir.clone());
        e.enabled = false;
        save(&e).unwrap();
        let (enabled, rules) = load(&dir, 15);
        assert!(!enabled);
        assert_eq!(rules, e.rules);
        let junk = r#"{"rules":[
            {"id":"x","match":{"kind":"name","value":"csrss.exe"},"priority":"high"},
            {"id":"y","match":{"kind":"name","value":"ok.exe"},"priority":"realtime"},
            {"id":"z","match":{"kind":"name","value":"fine.exe"},"priority":"low"}]}"#;
        fs::write(dir.join(FILE_NAME), junk).unwrap();
        let (enabled, rules) = load(&dir, 15);
        assert!(enabled, "defaults to on");
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].id, "z");
        let _ = fs::remove_dir_all(&dir);
    }

    // ---- against a real process

    use std::os::windows::process::CommandExt;
    use windows::Win32::System::Threading::{
        ABOVE_NORMAL_PRIORITY_CLASS, BELOW_NORMAL_PRIORITY_CLASS, HIGH_PRIORITY_CLASS,
        IDLE_PRIORITY_CLASS, NORMAL_PRIORITY_CLASS,
    };

    /// A scratch copy of PING.EXE, killed and deleted when dropped.
    struct Scratch {
        dir: PathBuf,
        child: std::process::Child,
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
            let _ = fs::remove_dir_all(&self.dir);
        }
    }

    fn class_of(pid: u32) -> u32 {
        let h = actions::open_process(pid, PROCESS_QUERY_LIMITED_INFORMATION).unwrap();
        unsafe { GetPriorityClass(h.0) }
    }

    fn mask_of(pid: u32) -> u64 {
        let h = actions::open_process(pid, PROCESS_QUERY_LIMITED_INFORMATION).unwrap();
        actions::read_affinity(h.0).unwrap().0
    }

    fn start_of(pid: u32) -> u64 {
        let h = actions::open_process(pid, PROCESS_QUERY_LIMITED_INFORMATION).unwrap();
        actions::filetime_to_unix(actions::handle_creation_filetime(h.0).unwrap())
    }

    /// The scratch process only: this test never touches any other process.
    #[test]
    fn engine_against_a_real_scratch_process() {
        let dir = std::env::temp_dir().join(format!("orbit-rules-live-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
        let exe = dir.join("orbit-rule-test.exe");
        fs::copy(format!(r"{root}\System32\PING.EXE"), &exe).unwrap();
        let child = std::process::Command::new(&exe)
            .args(["-n", "600", "127.0.0.1"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .stdin(std::process::Stdio::null())
            .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
            .spawn()
            .unwrap();
        let pid = child.id();
        let _scratch = Scratch { dir: dir.clone(), child };
        let r = {
            let mut r = row(pid, "orbit-rule-test.exe", exe.to_str());
            r.start_time = start_of(pid);
            r
        };
        let rows = vec![r];
        let (_, system) = actions::read_affinity(unsafe { GetCurrentProcess() }).unwrap();
        let mut e = Engine::new(false, system);
        e.initialized = true;
        e.rules = vec![rule("t", "name", "orbit-rule-test.exe", Some("high"), Some("1"))];
        let own = std::process::id();
        let set = |class| {
            let name = actions::priority_name(class).unwrap();
            actions::set_priority(pid, name.to_string(), None).unwrap();
        };

        // Start at normal, apply: High and one core.
        set(NORMAL_PRIORITY_CLASS.0);
        assert!(step(&mut e, &rows, 0, &RealOps, own));
        assert_eq!(class_of(pid), HIGH_PRIORITY_CLASS.0, "set to High");
        assert_eq!(mask_of(pid), 1, "limited to core 0");
        assert_eq!(e.statuses()[0].state, "applied");
        println!("scratch pid {pid}: applied -> High, core mask 1");

        // Something changes the cores back: re-applied (1st revert).
        let h = actions::open_process(pid, windows::Win32::System::Threading::PROCESS_SET_INFORMATION | PROCESS_QUERY_LIMITED_INFORMATION).unwrap();
        unsafe { windows::Win32::System::Threading::SetProcessAffinityMask(h.0, system as usize) }.unwrap();
        drop(h);
        assert_ne!(mask_of(pid), 1);
        step(&mut e, &rows, 10_000, &RealOps, own);
        assert_eq!(mask_of(pid), 1, "affinity re-applied");
        // Something changes the priority back: re-applied (2nd revert).
        set(BELOW_NORMAL_PRIORITY_CLASS.0);
        step(&mut e, &rows, 20_000, &RealOps, own);
        assert_eq!(class_of(pid), HIGH_PRIORITY_CLASS.0, "priority re-applied");
        assert_eq!(e.statuses()[0].state, "applied");

        // 3rd revert inside a minute: Orbit stops and leaves it alone.
        set(IDLE_PRIORITY_CLASS.0);
        step(&mut e, &rows, 30_000, &RealOps, own);
        assert_eq!(class_of(pid), IDLE_PRIORITY_CLASS.0, "not set a third time");
        assert_eq!(e.statuses()[0].state, "gaveUp");
        assert_eq!(e.statuses()[0].reason.as_deref(), Some(CHANGED_BACK));
        set(ABOVE_NORMAL_PRIORITY_CLASS.0);
        step(&mut e, &rows, 40_000, &RealOps, own);
        assert_eq!(class_of(pid), ABOVE_NORMAL_PRIORITY_CLASS.0, "no fight after giving up");
        println!("scratch pid {pid}: log = {:?}", e.log.iter().map(|l| (&l.kind, &l.text)).collect::<Vec<_>>());
    }
}
