//! Tests for `rules.rs`: pure parts, the engine against a fake OS, and one
//! test against a scratch copy of PING.EXE (never any other process).

use super::*;
use std::cell::RefCell;

const ME: u32 = 99;
const ALL: u64 = 0b1111;

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
        ..Rule::default()
    }
}

fn front_rule(id: &str, name: &str) -> Rule {
    Rule { in_front: true, behind: Some("normal".into()), ..rule(id, "name", name, Some("high"), None) }
}

#[derive(Default)]
struct Fake {
    prio: RefCell<HashMap<u32, String>>,
    mask: RefCell<HashMap<u32, u64>>,
    eco: RefCell<HashMap<u32, bool>>,
    /// Efficiency mode can't be read (older Windows): `read` gives None.
    eco_unreadable: bool,
    fail: RefCell<Option<String>>,
    elevated: RefCell<HashMap<u32, bool>>,
    sets: RefCell<u32>,
}

impl Fake {
    fn with(pids: &[u32], prio: &str) -> Self {
        let f = Fake::default();
        for &p in pids {
            f.prio.borrow_mut().insert(p, prio.into());
            f.mask.borrow_mut().insert(p, ALL);
        }
        f
    }
    fn p(&self, pid: u32) -> String {
        self.prio.borrow()[&pid].clone()
    }
}

impl Ops for Fake {
    fn guard(&self, _: u32) -> Result<(), String> {
        Ok(())
    }
    fn read(&self, pid: u32, _: u64, eco: bool) -> Result<Current, String> {
        let priority = self.prio.borrow().get(&pid).cloned().ok_or_else(|| EXITED.to_string())?;
        let mask = self.mask.borrow().get(&pid).copied().unwrap_or(ALL);
        let eco = if eco && !self.eco_unreadable { Some(self.eco.borrow().get(&pid).copied().unwrap_or(false)) } else { None };
        Ok(Current { priority, mask, eco })
    }
    fn set_priority(&self, pid: u32, _: u64, name: &str) -> Result<(), String> {
        assert_ne!(name, "realtime", "Realtime must never reach the OS");
        if let Some(e) = self.fail.borrow().clone() {
            return Err(e);
        }
        *self.sets.borrow_mut() += 1;
        self.prio.borrow_mut().insert(pid, name.to_string());
        Ok(())
    }
    fn set_affinity(&self, pid: u32, _: u64, mask: u64) -> Result<(), String> {
        if let Some(e) = self.fail.borrow().clone() {
            return Err(e);
        }
        *self.sets.borrow_mut() += 1;
        self.mask.borrow_mut().insert(pid, mask);
        Ok(())
    }
    fn set_eco(&self, pid: u32, _: u64, on: bool) -> Result<(), String> {
        *self.sets.borrow_mut() += 1;
        self.eco.borrow_mut().insert(pid, on);
        Ok(())
    }
    fn elevated(&self, pid: u32) -> Option<bool> {
        self.elevated.borrow().get(&pid).copied()
    }
}

fn engine_with(rules: Vec<Rule>, admin: bool) -> Engine {
    let mut e = Engine::new(admin, ALL, true);
    e.rules = rules;
    e.initialized = true;
    e
}

fn tick(e: &mut Engine, rows: &[ProcessRow], now: u64, ops: &Fake) -> bool {
    step(e, rows, now, ops, ME, false)
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
    let rules = vec![rule("n", "name", "game.exe", Some("low"), None), rule("p", "path", r"C:\Games\game.exe", Some("high"), None)];
    let r = row(5, "game.exe", Some(r"C:\Games\game.exe"));
    assert_eq!(find_rule(&rules, &r, false).unwrap().id, "p");
    let other = row(6, "game.exe", Some(r"D:\game.exe"));
    assert_eq!(find_rule(&rules, &other, false).unwrap().id, "n");
}

#[test]
fn disabled_and_boost_only_rules_match_only_when_in_force() {
    let mut r = rule("n", "name", "game.exe", Some("low"), None);
    r.enabled = false;
    assert!(find_rule(&[r], &row(5, "game.exe", None), true).is_none());
    let b = Rule { boost_only: true, ..rule("b", "name", "game.exe", Some("high"), None) };
    assert!(find_rule(std::slice::from_ref(&b), &row(5, "game.exe", None), false).is_none());
    assert!(find_rule(&[b], &row(5, "game.exe", None), true).is_some());
}

// ---- validation

#[test]
fn validation_cleans_and_rejects() {
    let ok = validate(&rule("", "name", "  game.exe ", Some("high"), Some("3")), &[], ALL, true).unwrap();
    assert_eq!(ok.matcher.value, "game.exe");
    assert_eq!(ok.affinity.as_deref(), Some("3"));
    let err = |r: Rule| validate(&r, &[], ALL, true).unwrap_err();
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
    let err = |m: &str| validate(&rule("", "name", "g.exe", None, Some(m)), &[], ALL, true).unwrap_err();
    assert_eq!(err("0"), "Pick at least one processor.");
    assert!(err("16").contains("processors this PC does not have"));
    assert!(err("abc").contains("not valid"));
    assert!(validate(&rule("", "name", "g.exe", None, Some("15")), &[], ALL, true).is_ok());
}

#[test]
fn duplicates_are_refused_but_editing_yourself_is_fine() {
    let a = rule("a", "name", "Game.exe", Some("high"), None);
    let dup = rule("", "name", "game.EXE", Some("low"), None);
    assert!(validate(&dup, std::slice::from_ref(&a), ALL, true).unwrap_err().contains("already a rule"));
    let edit = rule("a", "name", "game.exe", Some("low"), None);
    assert!(validate(&edit, &[a], ALL, true).is_ok());
}

#[test]
fn in_front_rules_need_a_priority_and_a_different_one_behind() {
    let ok = validate(&Rule { behind: None, ..front_rule("", "g.exe") }, &[], ALL, true).unwrap();
    assert_eq!(ok.behind.as_deref(), Some("normal"), "behind defaults to Normal");
    let below = validate(&Rule { behind: Some("belowNormal".into()), ..front_rule("", "g.exe") }, &[], ALL, true).unwrap();
    assert_eq!(below.behind.as_deref(), Some("belowNormal"));
    let err = |r: Rule| validate(&r, &[], ALL, true).unwrap_err();
    assert!(err(Rule { priority: None, ..front_rule("", "g.exe") }).contains("in front"));
    assert!(err(Rule { behind: Some("high".into()), ..front_rule("", "g.exe") }).contains("Normal or Below normal"));
    assert!(err(Rule { behind: Some("realtime".into()), ..front_rule("", "g.exe") }).contains("Realtime"));
    assert!(err(Rule { priority: Some("normal".into()), ..front_rule("", "g.exe") }).contains("different"));
    // A plain rule keeps no "behind" value.
    let plain = validate(&Rule { behind: Some("normal".into()), ..rule("", "name", "g.exe", Some("high"), None) }, &[], ALL, true).unwrap();
    assert_eq!(plain.behind, None);
}

#[test]
fn efficiency_mode_needs_windows_11_and_sets_low_itself() {
    let eco = Rule { efficiency: true, ..rule("", "name", "g.exe", None, None) };
    assert_eq!(validate(&eco, &[], ALL, false).unwrap_err(), NO_ECO);
    let ok = validate(&Rule { priority: Some("low".into()), ..eco.clone() }, &[], ALL, true).unwrap();
    assert_eq!(ok.priority, None, "Low is implied");
    assert!(validate(&Rule { priority: Some("high".into()), ..eco.clone() }, &[], ALL, true).unwrap_err().contains("Low priority"));
    assert!(validate(&Rule { in_front: true, priority: Some("high".into()), ..eco }, &[], ALL, true).unwrap_err().contains("in front"));
}

#[test]
fn summaries_read_plainly() {
    assert_eq!(summary(&rule("", "name", "g.exe", Some("high"), Some("3"))), "High priority, 2 cores");
    assert_eq!(summary(&front_rule("", "g.exe")), "High priority in front, Normal behind");
    assert_eq!(summary(&Rule { efficiency: true, boost_only: true, ..rule("", "name", "g.exe", None, None) }), "Efficiency mode, while Boost is on");
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
    for n in ["MsMpEng.exe", "NisSrv.exe", "MpDefenderCoreService.exe", "SecurityHealthService.exe", r"C:\ProgramData\Microsoft\Windows Defender\Platform\4.18\MsMpEngCP.exe"] {
        assert_eq!(refusal(n), Some(WHY_DEFENDER), "{n}");
    }
    for n in ["vgc.exe", "vgk.sys", "EasyAntiCheat.exe", "EasyAntiCheat_EOS.exe", "BEService.exe", "BEService_x64.exe", "BEDaisy.sys", "EAAntiCheat.GameService.exe", "FACEIT.exe", "PnkBstrA.exe"] {
        assert_eq!(refusal(n), Some(WHY_ANTICHEAT), "{n}");
    }
    assert_eq!(refusal(r"C:\Windows\System32\notepad.exe"), Some(WHY_WINDOWS));
    assert_eq!(refusal("League of Legends.exe"), None);
    assert_eq!(refusal(r"D:\Games\game.exe"), None);
    // Refused by validate() too, with the plain reason.
    assert_eq!(validate(&rule("", "name", "csrss.exe", Some("high"), None), &[], ALL, true).unwrap_err(), WHY_WINDOWS);
    assert_eq!(validate(&rule("", "name", "vgc.exe", Some("high"), None), &[], ALL, true).unwrap_err(), WHY_ANTICHEAT);
    assert_eq!(anticheat_product("vgc.exe"), Some("Riot Vanguard"));
    assert_eq!(anticheat_product("BEService.exe"), Some("BattlEye"));
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

// ---- Realtime, everywhere

#[test]
fn realtime_never_comes_out_of_a_rule() {
    for p in PRIORITIES.iter().chain(BEHIND.iter()) {
        assert_ne!(*p, "realtime");
    }
    for front in [true, false] {
        for r in [front_rule("a", "g.exe"), rule("a", "name", "g.exe", Some("realtime"), None), Rule { efficiency: true, ..Rule::default() }] {
            assert_ne!(want_of(&r, front).priority, Some("realtime"));
        }
    }
    let ops = Fake::with(&[10], "normal");
    let want = Want { priority: Some("realtime"), mask: None, eco: None };
    assert_eq!(apply(&want, 10, 0, &ops).unwrap_err(), NO_REALTIME);
    assert_eq!(RealOps.set_priority(10, 0, "realtime").unwrap_err(), NO_REALTIME);
}

// ---- engine with fake OS calls

#[test]
fn a_new_matching_process_is_set_in_the_same_tick() {
    let mut e = engine_with(vec![rule("a", "name", "game.exe", Some("high"), Some("3"))], true);
    let ops = Fake::with(&[10], "low");
    let rows = vec![row(10, "game.exe", Some(r"C:\g\game.exe")), row(11, "other.exe", None)];
    assert!(tick(&mut e, &rows, 5_000, &ops));
    assert_eq!(ops.p(10), "high");
    assert_eq!(ops.mask.borrow()[&10], 3);
    let st = e.statuses();
    assert_eq!(st.len(), 1);
    assert_eq!((st[0].pid, st[0].state.as_str(), st[0].summary.as_str()), (10, "applied", "High priority, 2 cores"));
    assert_eq!(e.log.back().unwrap().kind, "applied");
    assert_eq!(e.stats["a"].last_applied, Some(5_000));
    // Nothing to do next tick.
    assert!(!tick(&mut e, &rows, 6_000, &ops));
    assert_eq!(*ops.sets.borrow(), 2);
}

#[test]
fn an_app_already_at_the_value_is_logged_not_touched() {
    let mut e = engine_with(vec![rule("a", "name", "game.exe", Some("high"), None)], true);
    let ops = Fake::with(&[10], "high");
    tick(&mut e, &[row(10, "game.exe", None)], 1, &ops);
    assert_eq!(*ops.sets.borrow(), 0);
    assert!(e.log.back().unwrap().text.starts_with("Already set"));
}

#[test]
fn reapplies_twice_then_stops_on_the_third_revert() {
    let mut e = engine_with(vec![rule("a", "name", "game.exe", Some("high"), None)], true);
    let ops = Fake::with(&[10], "low");
    let rows = vec![row(10, "game.exe", None)];
    tick(&mut e, &rows, 0, &ops);
    for (i, t) in [10_000u64, 20_000].into_iter().enumerate() {
        ops.prio.borrow_mut().insert(10, "normal".into());
        assert!(tick(&mut e, &rows, t, &ops), "revert {i}");
        assert_eq!(ops.p(10), "high");
        assert_eq!(e.statuses()[0].state, "applied");
    }
    ops.prio.borrow_mut().insert(10, "normal".into());
    assert!(tick(&mut e, &rows, 30_000, &ops));
    assert_eq!(ops.p(10), "normal", "not set a third time");
    let s = &e.statuses()[0];
    assert_eq!((s.state.as_str(), s.reason.as_deref()), ("gaveUp", Some(CHANGED_BACK)));
    assert_eq!(e.stats["a"].last_failed.as_ref().unwrap().reason, CHANGED_BACK);
    // Stays stopped, and checks nothing more for this run.
    ops.prio.borrow_mut().insert(10, "low".into());
    assert!(!tick(&mut e, &rows, 40_000, &ops));
    assert_eq!(ops.p(10), "low");
    // A new run of the app (new start time) gets the rule again.
    let mut again = row(10, "game.exe", None);
    again.start_time += 500;
    tick(&mut e, &[again], 50_000, &ops);
    assert_eq!(ops.p(10), "high");
}

#[test]
fn an_affinity_that_changes_back_is_set_again() {
    let mut e = engine_with(vec![rule("a", "name", "game.exe", None, Some("3"))], true);
    let ops = Fake::with(&[10], "normal");
    let rows = vec![row(10, "game.exe", None)];
    tick(&mut e, &rows, 0, &ops);
    ops.mask.borrow_mut().insert(10, ALL);
    tick(&mut e, &rows, 5_000, &ops);
    assert_eq!(ops.mask.borrow()[&10], 3);
    assert!(e.log.back().unwrap().text.contains("processors"));
}

#[test]
fn access_denied_says_why_once_and_does_not_retry() {
    // (Orbit admin, target elevated, anti-cheat running) -> reason, needs admin
    let vanguard = "Riot Vanguard (anti-cheat) blocked this change. Orbit won't keep trying.";
    let cases: [(bool, Option<bool>, bool, &str, bool); 6] = [
        (false, Some(true), false, NEEDS_ADMIN, true),
        (false, None, false, NEEDS_ADMIN, true),
        (false, Some(false), false, BLOCKED, false),
        (true, None, false, BLOCKED, false),
        (false, Some(false), true, vanguard, false),
        (true, Some(true), true, vanguard, false),
    ];
    for (admin, elevated, vgc, want, flag) in cases {
        let mut e = engine_with(vec![rule("a", "name", "game.exe", Some("high"), None)], admin);
        let ops = Fake::with(&[10, 50], "normal");
        if let Some(v) = elevated {
            ops.elevated.borrow_mut().insert(10, v);
        }
        *ops.fail.borrow_mut() = Some(ACCESS_DENIED.to_string());
        let mut rows = vec![row(10, "game.exe", None)];
        if vgc {
            rows.push(row(50, "vgc.exe", None));
        }
        assert!(tick(&mut e, &rows, 1, &ops));
        let s = &e.statuses()[0];
        assert_eq!((s.state.as_str(), s.reason.as_deref(), s.needs_admin), ("failed", Some(want), flag), "{admin} {elevated:?} {vgc}");
        let entry = e.log.back().unwrap();
        assert_eq!((entry.kind.as_str(), entry.needs_admin), ("blocked", flag));
        assert_eq!(e.stats["a"].last_failed.as_ref().unwrap().reason, want);
        // No retry on later ticks, no second log line.
        *ops.fail.borrow_mut() = None;
        assert!(!tick(&mut e, &rows, 2_000, &ops));
        assert_eq!(e.log.len(), 1);
        assert_eq!(*ops.sets.borrow(), 0);
    }
}

#[test]
fn exited_processes_are_forgotten_quietly() {
    let mut e = engine_with(vec![rule("a", "name", "game.exe", Some("high"), None)], true);
    let ops = Fake::with(&[10], "low");
    tick(&mut e, &[row(10, "game.exe", None)], 0, &ops);
    assert_eq!(e.tracked.len(), 1);
    assert!(tick(&mut e, &[], 1_000, &ops));
    assert!(e.tracked.is_empty());
    // The pid vanished from the OS before the new-process apply: no log line.
    let mut e = engine_with(vec![rule("a", "name", "game.exe", Some("high"), None)], true);
    tick(&mut e, &[row(77, "game.exe", None)], 0, &Fake::default());
    assert!(e.log.is_empty() && e.tracked.is_empty());
}

#[test]
fn never_touches_orbit_windows_rows_refused_names_or_system_paths() {
    let mut e = engine_with(
        vec![
            rule("a", "name", "game.exe", Some("high"), None),
            rule("b", "name", "csrss.exe", Some("high"), None),
            rule("c", "name", "vgc.exe", Some("high"), None),
            // A name rule that would match Windows' own copy: not applied there.
            rule("d", "name", "notepad.exe", Some("high"), None),
        ],
        true,
    );
    let mut win = row(20, "game.exe", None);
    win.kind = ProcessKind::Windows;
    let mut notepad = row(23, "notepad.exe", Some(r"C:\Windows\System32\notepad.exe"));
    notepad.kind = ProcessKind::App; // it has a window
    let rows = vec![row(ME, "game.exe", None), win, row(21, "csrss.exe", None), row(22, "vgc.exe", None), row(3, "game.exe", None), notepad];
    let ops = Fake::with(&rows.iter().map(|r| r.pid).collect::<Vec<_>>(), "normal");
    tick(&mut e, &rows, 0, &ops);
    assert_eq!(*ops.sets.borrow(), 0);
    assert!(e.statuses().is_empty());
    // A copy of notepad outside Windows is fine.
    let mine = row(24, "notepad.exe", Some(r"D:\Tools\notepad.exe"));
    ops.prio.borrow_mut().insert(24, "normal".into());
    tick(&mut e, &[mine], 1, &ops);
    assert_eq!(ops.p(24), "high");
}

#[test]
fn the_master_switch_and_disabled_rules_stop_everything() {
    let mut e = engine_with(vec![rule("a", "name", "game.exe", Some("high"), None)], true);
    let ops = Fake::with(&[10], "low");
    let rows = vec![row(10, "game.exe", None)];
    tick(&mut e, &rows, 0, &ops);
    e.enabled = false;
    assert!(tick(&mut e, &rows, 1_000, &ops), "statuses cleared");
    assert!(e.statuses().is_empty());
    ops.prio.borrow_mut().insert(10, "low".into());
    tick(&mut e, &rows, 2_000, &ops);
    assert_eq!(ops.p(10), "low");
    e.enabled = true;
    e.reset_tracking();
    tick(&mut e, &rows, 3_000, &ops);
    assert_eq!(ops.p(10), "high");
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

// ---- only while in front

#[test]
fn in_front_switches_with_the_window_and_is_not_a_revert() {
    let mut e = engine_with(vec![front_rule("f", "game.exe")], true);
    // Two processes of the same game, and another app.
    let ops = Fake::with(&[10, 11, 30], "normal");
    let rows = vec![row(10, "game.exe", None), row(11, "game.exe", None), row(30, "chat.exe", None)];
    e.foreground = 30;
    tick(&mut e, &rows, 0, &ops);
    assert_eq!((ops.p(10), ops.p(11)), ("normal".into(), "normal".into()), "behind: Normal");
    assert!(e.log.back().unwrap().text.contains("behind"));

    // The game's window comes to the front: both its processes go High now.
    front_changed(&mut e, 10, 1_000, &ops);
    assert_eq!((ops.p(10), ops.p(11)), ("high".into(), "high".into()));
    // Back to the chat app, then to the game again, many times: never "changed back".
    for i in 0..6u64 {
        front_changed(&mut e, if i % 2 == 0 { 30 } else { 11 }, 2_000 + i, &ops);
        assert!(!tick(&mut e, &rows, 2_000 + i, &ops));
    }
    assert_eq!(ops.p(10), "high", "game in front");
    assert!(e.tracked.values().all(|t| t.reverts.is_empty() && t.state == State::Applied));

    // Something else lowers it while in front: that is a revert.
    ops.prio.borrow_mut().insert(10, "low".into());
    assert!(tick(&mut e, &rows, 9_000, &ops));
    assert_eq!(ops.p(10), "high");
    assert_eq!(e.tracked[&(10, 1010)].reverts.len(), 1);
    assert_eq!(ops.p(30), "normal", "the other app is never touched");
}

#[test]
fn in_front_without_the_hook_catches_up_on_the_next_tick() {
    let mut e = engine_with(vec![front_rule("f", "game.exe")], true);
    let ops = Fake::with(&[10], "low");
    let rows = vec![row(10, "game.exe", None)];
    // The game is already in front when Orbit first sees it.
    e.foreground = 10;
    tick(&mut e, &rows, 0, &ops);
    assert_eq!(ops.p(10), "high");
    // Foreground moved without the hook running (it failed): the tick fixes it, no revert.
    e.foreground = 0;
    tick(&mut e, &rows, 1_000, &ops);
    assert_eq!(ops.p(10), "normal");
    assert!(e.tracked[&(10, 1010)].reverts.is_empty());
}

// ---- Boost link

#[test]
fn boost_only_rules_apply_while_boost_is_on_and_put_back_after() {
    let mut e = engine_with(vec![Rule { boost_only: true, ..rule("b", "name", "game.exe", Some("high"), Some("3")) }], true);
    let ops = Fake::with(&[10], "normal");
    let rows = vec![row(10, "game.exe", None)];
    step(&mut e, &rows, 0, &ops, ME, false);
    assert_eq!(ops.p(10), "normal", "Boost off: not applied");
    assert!(step(&mut e, &rows, 1_000, &ops, ME, true), "Boost on: same tick");
    assert_eq!((ops.p(10), ops.mask.borrow()[&10]), ("high".into(), 3));
    assert!(step(&mut e, &rows, 2_000, &ops, ME, false));
    assert_eq!((ops.p(10), ops.mask.borrow()[&10]), ("normal".into(), ALL), "put back");
    let last = e.log.back().unwrap();
    assert_eq!((last.kind.as_str(), last.text.as_str()), ("reset", "Boost ended. Put back to Normal priority and all cores."));
    assert!(e.statuses().is_empty());
    // Boost again: applied again.
    step(&mut e, &rows, 3_000, &ops, ME, true);
    assert_eq!(ops.p(10), "high");
}

// ---- efficiency mode

#[test]
fn efficiency_mode_sets_low_and_eco_and_watches_both() {
    let mut e = engine_with(vec![Rule { efficiency: true, ..rule("e", "name", "bg.exe", None, None) }], true);
    let ops = Fake::with(&[10], "normal");
    let rows = vec![row(10, "bg.exe", None)];
    tick(&mut e, &rows, 0, &ops);
    assert_eq!((ops.p(10), ops.eco.borrow()[&10]), ("low".into(), true));
    assert_eq!(e.log.back().unwrap().text, "Set to Efficiency mode.");
    ops.eco.borrow_mut().insert(10, false);
    tick(&mut e, &rows, 1_000, &ops);
    assert!(ops.eco.borrow()[&10], "turned on again");
    assert!(e.log.back().unwrap().text.contains("Efficiency mode was turned off"));
    // Where it can't be read, only the priority is watched.
    let mut e = engine_with(vec![Rule { efficiency: true, ..rule("e", "name", "bg.exe", None, None) }], true);
    let ops = Fake { eco_unreadable: true, ..Fake::with(&[10], "normal") };
    tick(&mut e, &rows, 0, &ops);
    assert!(ops.eco.borrow()[&10], "set once");
    ops.eco.borrow_mut().insert(10, false);
    assert!(!tick(&mut e, &rows, 1_000, &ops), "unreadable: no drift");
}

// ---- reset

#[test]
fn reset_puts_the_app_back_and_switches_the_rule_off() {
    let mut e = engine_with(vec![Rule { efficiency: true, ..rule("e", "name", "game.exe", None, Some("3")) }], true);
    let ops = Fake::with(&[10, 11], "normal");
    let rows = vec![row(10, "game.exe", None), row(11, "other.exe", None)];
    tick(&mut e, &rows, 0, &ops);
    assert_eq!(ops.p(10), "low");
    let (n, problems) = reset_rule(&mut e, "e", &rows, 1_000, &ops, ME).unwrap();
    assert_eq!((n, problems.len()), (1, 0));
    assert_eq!((ops.p(10), ops.mask.borrow()[&10], ops.eco.borrow()[&10]), ("normal".into(), ALL, false));
    assert!(!e.rules[0].enabled);
    assert_eq!(e.log.back().unwrap().text, "Put back to Normal priority, all cores and efficiency mode off. The rule is off.");
    assert!(e.statuses().is_empty());
    assert!(!tick(&mut e, &rows, 2_000, &ops), "off: not applied again");
    assert_eq!(ops.p(10), "normal");
    assert_eq!(ops.p(11), "normal", "other apps untouched");
    assert!(reset_rule(&mut e, "nope", &rows, 0, &ops, ME).is_err());
}

// ---- backup

#[test]
fn import_adds_replaces_and_skips_with_reasons() {
    let existing = vec![rule("a", "name", "game.exe", Some("high"), None), rule("b", "name", "chat.exe", Some("belowNormal"), None)];
    let incoming = vec![
        rule("x", "name", "Game.exe", Some("aboveNormal"), None), // replaces a
        rule("y", "name", "chat.exe", Some("belowNormal"), None), // same as b
        rule("z", "name", "new.exe", Some("high"), None),         // added
        rule("q", "name", "csrss.exe", Some("high"), None),       // refused
        rule("r", "name", "ok.exe", Some("realtime"), None),      // refused
        rule("s", "name", "new.exe", Some("low"), None),          // twice in the file
        Rule { efficiency: true, ..rule("t", "name", "bg.exe", None, None) }, // needs Windows 11 here
    ];
    let mut n = 0;
    let (out, report) = merge(&existing, &incoming, ALL, false, || {
        n += 1;
        format!("new{n}")
    });
    assert_eq!((report.added, report.replaced, report.unchanged), (1, 1, 1));
    assert_eq!(report.skipped.len(), 4, "{:?}", report.skipped);
    assert!(report.skipped.iter().any(|s| s == &format!("csrss.exe: {WHY_WINDOWS}")));
    assert!(report.skipped.iter().any(|s| s.contains("Realtime")));
    assert!(report.skipped.iter().any(|s| s.starts_with("bg.exe: ") && s.contains("Windows 11")));
    assert_eq!(out.len(), 3);
    assert_eq!((out[0].id.as_str(), out[0].priority.as_deref()), ("a", Some("aboveNormal")), "id kept");
    assert_eq!(out[2].id, "new1");
}

#[test]
fn import_stops_at_the_rule_limit() {
    let existing: Vec<Rule> = (0..MAX_RULES).map(|i| rule(&format!("r{i}"), "name", &format!("app{i}.exe"), Some("high"), None)).collect();
    let (out, report) = merge(&existing, &[rule("", "name", "one-more.exe", Some("high"), None)], ALL, true, || "x".into());
    assert_eq!((out.len(), report.added), (MAX_RULES, 0));
    assert!(report.skipped[0].contains("at most"));
}

// ---- files

#[test]
fn rules_round_trip_through_the_file_and_bad_ones_are_dropped() {
    let dir = std::env::temp_dir().join(format!("orbit-rules-file-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    let mut e = engine_with(
        vec![
            rule("a", "name", "game.exe", Some("high"), Some("3")),
            Rule { boost_only: true, ..front_rule("b", "other.exe") },
            Rule { efficiency: true, ..rule("c", "name", "bg.exe", None, None) },
        ],
        true,
    );
    e.dir = Some(dir.clone());
    e.enabled = false;
    save(&e).unwrap();
    let (enabled, rules) = load(&dir, ALL, true);
    assert!(!enabled);
    assert_eq!(rules, e.rules);
    // On a PC without efficiency mode, that rule is dropped.
    assert_eq!(load(&dir, ALL, false).1.len(), 2);
    // A 0.9 part 1 file (no new fields) still loads, and junk is dropped.
    let junk = r#"{"rules":[
        {"id":"x","match":{"kind":"name","value":"csrss.exe"},"priority":"high","enabled":true},
        {"id":"y","match":{"kind":"name","value":"ok.exe"},"priority":"realtime","enabled":true},
        {"id":"z","match":{"kind":"name","value":"fine.exe"},"priority":"low","enabled":true},
        {"id":"z","match":{"kind":"name","value":"again.exe"},"priority":"low","enabled":true}]}"#;
    fs::write(dir.join(FILE_NAME), junk).unwrap();
    let (enabled, rules) = load(&dir, ALL, true);
    assert!(enabled, "defaults to on");
    assert_eq!(rules.len(), 1);
    assert_eq!((rules[0].id.as_str(), rules[0].in_front, rules[0].boost_only, rules[0].efficiency), ("z", false, false, false));
    let _ = fs::remove_dir_all(&dir);
}

// ---- against a real process

use std::os::windows::process::CommandExt;
use windows::Win32::System::Threading::{
    ABOVE_NORMAL_PRIORITY_CLASS, BELOW_NORMAL_PRIORITY_CLASS, HIGH_PRIORITY_CLASS, IDLE_PRIORITY_CLASS,
    NORMAL_PRIORITY_CLASS,
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

    // The light tick's start time is the sampler's (sysinfo's), so a process
    // keeps its key when updates are paused and resumed.
    let mut sys = sysinfo::System::new();
    sys.refresh_processes(sysinfo::ProcessesToUpdate::Some(&[sysinfo::Pid::from_u32(pid)]), true);
    let sampled = sys.process(sysinfo::Pid::from_u32(pid)).map(|p| p.start_time());
    assert_eq!(sampled, Some(start_of(pid)), "light tick and sampler agree on the start time");

    let r = {
        let mut r = row(pid, "orbit-rule-test.exe", exe.to_str());
        r.start_time = start_of(pid);
        r
    };
    let rows = vec![r];
    let (_, system) = actions::read_affinity(unsafe { GetCurrentProcess() }).unwrap();
    let mut e = Engine::new(false, system, false);
    e.initialized = true;
    e.rules = vec![rule("t", "name", "orbit-rule-test.exe", Some("high"), Some("1"))];
    let own = std::process::id();
    let set = |class| {
        let name = actions::priority_name(class).unwrap();
        actions::set_priority(pid, name.to_string(), None).unwrap();
    };

    // Start at normal, apply: High and one core.
    set(NORMAL_PRIORITY_CLASS.0);
    assert!(step(&mut e, &rows, 0, &RealOps, own, false));
    assert_eq!(class_of(pid), HIGH_PRIORITY_CLASS.0, "set to High");
    assert_eq!(mask_of(pid), 1, "limited to core 0");
    assert_eq!(e.statuses()[0].state, "applied");
    println!("scratch pid {pid}: applied -> High, core mask 1");

    // Something changes the cores back: re-applied (1st revert).
    let h = actions::open_process(pid, PROCESS_SET_INFORMATION | PROCESS_QUERY_LIMITED_INFORMATION).unwrap();
    unsafe { windows::Win32::System::Threading::SetProcessAffinityMask(h.0, system as usize) }.unwrap();
    drop(h);
    assert_ne!(mask_of(pid), 1);
    step(&mut e, &rows, 10_000, &RealOps, own, false);
    assert_eq!(mask_of(pid), 1, "affinity re-applied");
    // Something changes the priority back: re-applied (2nd revert).
    set(BELOW_NORMAL_PRIORITY_CLASS.0);
    step(&mut e, &rows, 20_000, &RealOps, own, false);
    assert_eq!(class_of(pid), HIGH_PRIORITY_CLASS.0, "priority re-applied");
    assert_eq!(e.statuses()[0].state, "applied");

    // 3rd revert inside a minute: Orbit stops and leaves it alone.
    set(IDLE_PRIORITY_CLASS.0);
    step(&mut e, &rows, 30_000, &RealOps, own, false);
    assert_eq!(class_of(pid), IDLE_PRIORITY_CLASS.0, "not set a third time");
    assert_eq!(e.statuses()[0].state, "gaveUp");
    assert_eq!(e.statuses()[0].reason.as_deref(), Some(CHANGED_BACK));
    set(ABOVE_NORMAL_PRIORITY_CLASS.0);
    step(&mut e, &rows, 40_000, &RealOps, own, false);
    assert_eq!(class_of(pid), ABOVE_NORMAL_PRIORITY_CLASS.0, "no fight after giving up");

    // Reset: Normal and every core, rule off.
    let (n, problems) = reset_rule(&mut e, "t", &rows, 50_000, &RealOps, own).unwrap();
    assert_eq!((n, problems), (1, Vec::<String>::new()));
    assert_eq!((class_of(pid), mask_of(pid)), (NORMAL_PRIORITY_CLASS.0, system), "reset");
    assert!(!e.rules[0].enabled);

    // In front only: behind gives Normal, the window "coming forward" gives High.
    e.rules = vec![front_rule("f", "orbit-rule-test.exe")];
    e.reset_tracking();
    e.foreground = 1;
    set(IDLE_PRIORITY_CLASS.0);
    step(&mut e, &rows, 60_000, &RealOps, own, false);
    assert_eq!(class_of(pid), NORMAL_PRIORITY_CLASS.0, "behind: Normal");
    front_changed(&mut e, pid, 61_000, &RealOps);
    assert_eq!(class_of(pid), HIGH_PRIORITY_CLASS.0, "in front: High");
    front_changed(&mut e, 1, 62_000, &RealOps);
    assert_eq!(class_of(pid), NORMAL_PRIORITY_CLASS.0, "behind again");
    assert!(e.tracked.values().all(|t| t.reverts.is_empty()));

    // Efficiency mode, where Windows has it.
    if is_windows_11() {
        e.eco_supported = true;
        e.rules = vec![Rule { efficiency: true, ..rule("e", "name", "orbit-rule-test.exe", None, None) }];
        e.reset_tracking();
        step(&mut e, &rows, 70_000, &RealOps, own, false);
        assert_eq!(class_of(pid), IDLE_PRIORITY_CLASS.0, "efficiency mode runs at Low");
        let cur = RealOps.read(pid, rows[0].start_time, true).unwrap();
        println!("scratch pid {pid}: efficiency mode read back as {:?}", cur.eco);
        assert_ne!(cur.eco, Some(false));
        reset_rule(&mut e, "e", &rows, 71_000, &RealOps, own).unwrap();
        assert_eq!(class_of(pid), NORMAL_PRIORITY_CLASS.0);
    } else {
        println!("efficiency mode not checked: this Windows is older than build 22000");
    }
    println!("scratch pid {pid}: log = {:?}", e.log.iter().map(|l| (&l.kind, &l.text)).collect::<Vec<_>>());
}
