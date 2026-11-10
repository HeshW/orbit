//! Debloat tests. Everything runs on a fake backend: nothing here touches the
//! PC's startup apps, tasks, services or apps. `real_analysis` (ignored) only
//! reads this PC and prints what Orbit would show.

use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use super::guard::{self, Subject};
use super::known::{self, glob, Known, KNOWN};
use super::*;

// ---------------------------------------------------------------- fake backend

#[derive(Default)]
struct Fake {
    inv: RefCell<Inventory>,
    admin: bool,
    calls: RefCell<Vec<String>>,
    /// Calls that fail ("task:\\Zoom...") with this message.
    fail: Vec<String>,
    /// Where the change log lives: each change call checks it was written first.
    dir: Option<PathBuf>,
    pending_seen: RefCell<Vec<String>>,
    installed: RefCell<Vec<String>>,
}

impl Fake {
    fn record(&self, call: String, target: &str) -> Result<(), String> {
        if let Some(dir) = &self.dir {
            let log = load_log(dir).expect("log readable");
            if log.changes.iter().any(|c| c.target == target && c.state == ChangeState::Pending) {
                self.pending_seen.borrow_mut().push(target.to_string());
            }
        }
        let failing = self.fail.iter().any(|f| call.starts_with(f));
        self.calls.borrow_mut().push(call);
        if failing {
            Err("Access denied. Try Restart as administrator.".into())
        } else {
            Ok(())
        }
    }
}

impl Backend for Fake {
    fn inventory(&self) -> Inventory {
        self.inv.borrow().clone()
    }
    fn is_admin(&self) -> bool {
        self.admin
    }
    fn set_startup(&self, id: &str, enabled: bool) -> Result<(), String> {
        self.record(format!("startup {id} {enabled}"), id)?;
        for e in self.inv.borrow_mut().startup.iter_mut().filter(|e| e.id == id) {
            e.enabled = enabled;
        }
        Ok(())
    }
    fn set_task(&self, path: &str, enabled: bool) -> Result<(), String> {
        self.record(format!("task {path} {enabled}"), path)?;
        for t in self.inv.borrow_mut().tasks.iter_mut().filter(|t| t.path == path) {
            t.enabled = enabled;
        }
        Ok(())
    }
    fn set_service_start(&self, name: &str, start: &str) -> Result<(), String> {
        self.record(format!("service {name} {start}"), name)?;
        for s in self.inv.borrow_mut().services.iter_mut().filter(|s| s.name == name) {
            s.start_type = start.to_string();
        }
        Ok(())
    }
    fn remove_app(&self, full_name: &str) -> Result<(), String> {
        self.record(format!("remove {full_name}"), full_name)?;
        self.inv.borrow_mut().apps.retain(|a| a.full_name != full_name);
        Ok(())
    }
    fn app_installed(&self, family: &str) -> bool {
        self.installed.borrow().iter().any(|f| f == family)
    }
    fn open_store(&self, family: &str) -> Result<(), String> {
        self.calls.borrow_mut().push(format!("store {family}"));
        Ok(())
    }
}

fn startup(id: &str, name: &str, path: &str, enabled: bool, needs_admin: bool) -> RawStartup {
    RawStartup {
        id: id.into(),
        name: name.into(),
        command: format!("\"{path}\""),
        path: Some(path.into()),
        enabled,
        needs_admin,
        impact: None,
        suggested: false,
    }
}

fn task(path: &str, exe: &str, enabled: bool, needs_admin: bool) -> RawTask {
    RawTask { path: path.into(), enabled, exe: exe.into(), author: None, needs_admin }
}

fn service(name: &str, display: &str, start: &str, path: &str, third_party: bool) -> RawService {
    RawService { name: name.into(), display: display.into(), start_type: start.into(), path: path.into(), third_party }
}

fn app(name: &str, publisher: &str) -> RawApp {
    RawApp {
        name: name.into(),
        full_name: format!("{name}_1.0.0.0_x64__{publisher}"),
        family: format!("{name}_{publisher}"),
        display: None,
        system: false,
    }
}

/// A PC with something in every verdict of every group.
fn sample() -> Inventory {
    Inventory {
        startup: vec![
            startup(r"HKCU\Run\Steam", "Steam", r"C:\Program Files (x86)\Steam\steam.exe", true, false),
            startup(r"HKLM\Run\SecurityHealth", "SecurityHealth", r"C:\Windows\system32\SecurityHealthSystray.exe", true, true),
            startup(r"HKCU\Run\Spotify", "Spotify", r"C:\Users\u\AppData\Roaming\Spotify\Spotify.exe", false, false),
            RawStartup { impact: Some("high".into()), suggested: true, ..startup(r"HKCU\Run\Gadget", "Gadget", r"C:\Program Files\Gadget\gadget.exe", true, false) },
            startup(r"HKLM\Run\Orbit", "Orbit", r"C:\Users\u\AppData\Local\Orbit\orbit.exe", true, true),
        ],
        tasks: vec![
            task(r"\Microsoft\Windows\Application Experience\Microsoft Compatibility Appraiser", r"C:\Windows\system32\compattelrunner.exe", true, true),
            task(r"\Microsoft\Windows\Defrag\ScheduledDefrag", r"C:\Windows\system32\defrag.exe", true, true),
            task(r"\ZoomUpdateTaskUser-S-1-5-21-1", r"C:\Users\u\AppData\Roaming\Zoom\bin\installer.exe", true, false),
            task(r"\MicrosoftEdgeUpdateTaskMachineCore", r"C:\Program Files (x86)\Microsoft\EdgeUpdate\MicrosoftEdgeUpdate.exe", true, true),
            task(r"\SomeVendor Helper", r"C:\Program Files\Vendor\helper.exe", true, false),
        ],
        services: vec![
            service("DiagTrack", "Connected User Experiences and Telemetry", "auto", r"C:\Windows\System32\diagtrack.dll", false),
            service("MapsBroker", "Downloaded Maps Manager", "autoDelayed", r"C:\Windows\System32\moshost.dll", false),
            service("SysMain", "SysMain", "auto", r"C:\Windows\system32\sysmain.dll", false),
            service("WinDefend", "Microsoft Defender Antivirus Service", "auto", r"C:\ProgramData\Microsoft\Windows Defender\MsMpEng.exe", true),
            service("VendorSvc", "Vendor Service", "auto", r"C:\Program Files\Vendor\svc.exe", true),
            service("Audiosrv", "Windows Audio", "auto", r"C:\Windows\System32\audiosrv.dll", false),
            service("TrkWks", "Distributed Link Tracking Client", "manual", r"C:\Windows\System32\trkwks.dll", false),
        ],
        apps: vec![
            app("Microsoft.BingWeather", "8wekyb3d8bbwe"),
            app("Clipchamp.Clipchamp", "yxz26nhyzhsrt"),
            app("Microsoft.WindowsStore", "8wekyb3d8bbwe"),
            app("Microsoft.WindowsCalculator", "8wekyb3d8bbwe"),
        ],
        errors: Vec::new(),
    }
}

fn temp_dir(tag: &str) -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let base = std::env::var_os("CARGO_TARGET_DIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    let dir = base.join(format!("orbit-debloat-test-{}-{tag}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn fake(admin: bool, dir: &PathBuf) -> Fake {
    Fake { inv: RefCell::new(sample()), admin, dir: Some(dir.clone()), ..Default::default() }
}

fn find<'a>(a: &'a Analysis, id: &str) -> &'a Item {
    a.groups.iter().flat_map(|g| g.items.iter()).find(|i| i.id == id).unwrap_or_else(|| panic!("no item {id}"))
}

fn ids(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

// ---------------------------------------------------------------- the list

#[test]
fn glob_matches_like_a_file_pattern() {
    assert!(glob("steam.exe", "Steam.EXE"));
    assert!(glob(r"\GoogleUpdateTask*", r"\GoogleUpdateTaskMachineUA{1234}"));
    assert!(glob("king.com.*", "king.com.CandyCrushSaga"));
    assert!(glob("*Orbit*", "LegacyTech.Orbit"));
    assert!(glob("a*b*c", "aXXbYYc"));
    assert!(!glob("steam.exe", "steamwebhelper.exe"));
    assert!(!glob(r"\Opera GX scheduled Autoupdate*", r"\Opera GX scheduled assistant Autoupdate 1"));
    assert!(!glob("a*b", "ac"));
}

#[test]
fn reviewed_list_is_short_and_complete() {
    assert!((40..=90).contains(&KNOWN.len()), "{} entries", KNOWN.len());
    for k in KNOWN {
        assert!(!k.patterns.is_empty(), "{}", k.name);
        assert!(!k.name.trim().is_empty() && !k.what.trim().is_empty() && !k.note.trim().is_empty(), "{}", k.name);
        for text in [k.what, k.note] {
            assert!(text.ends_with('.'), "{}: {text}", k.name);
            assert!(text.len() <= 110, "{}: too long: {text}", k.name);
            let lower = text.to_lowercase();
            for word in ["danger", "warning", "risk", "faster boot", "speed up", "boost"] {
                assert!(!lower.contains(word), "{}: scare or speed word in {text}", k.name);
            }
        }
    }
}

#[test]
fn reviewed_list_never_contradicts_the_guard() {
    for k in KNOWN.iter().filter(|k| k.verdict == Verdict::Off) {
        for p in k.patterns {
            let key = p.replace('*', "X");
            let path = match k.group {
                Group::Startup if key.contains('.') => format!(r"C:\Program Files\Vendor\{key}"),
                _ => String::new(),
            };
            let s = Subject { group: k.group, key: &key, display: "", path: &path, reviewed: true, system_package: false };
            assert_eq!(guard::refuse(&s), None, "{} ({p}) is Safe to turn off but the guard refuses it", k.name);
        }
    }
}

#[test]
fn docs_list_every_reviewed_entry() {
    let doc = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), r"\..\docs\features\debloat.md")).expect("docs/features/debloat.md");
    for k in KNOWN {
        assert!(doc.contains(k.name), "debloat.md lacks {}", k.name);
        for p in k.patterns {
            assert!(doc.contains(p), "debloat.md lacks pattern {p}");
        }
    }
}

// ---------------------------------------------------------------- the guard

fn refused(group: Group, key: &str, path: &str) -> bool {
    guard::refuse(&Subject { group, key, display: "", path, reviewed: false, system_package: false }).is_some()
}

#[test]
fn guard_refuses_the_never_offer_list() {
    for s in [
        "WinDefend", "WdNisSvc", "Sense", "MDCoreSvc", "SecurityHealthService", "wscsvc", "mpssvc", "BFE",
        "wuauserv", "UsoSvc", "WaaSMedicSvc", "BITS", "CryptSvc", "KeyIso", "Dnscache", "Dhcp", "NlaSvc",
        "nsi", "WlanSvc", "Audiosrv", "AudioEndpointBuilder", "Spooler", "RpcSs", "RpcEptMapper", "DcomLaunch",
        "PlugPlay", "Power", "ProfSvc", "EventLog", "Schedule", "Winmgmt", "W32Time", "AppXSvc", "ClipSVC",
        "InstallService", "NVDisplay.ContainerLocalSystem", "clr_optimization_v4.0.30319_64", "spooler",
    ] {
        assert!(refused(Group::Service, s, ""), "service {s}");
    }
    assert!(refused(Group::Service, "SomeDriver", r"C:\Windows\System32\drivers\x.sys"), "drivers");
    assert!(guard::refuse(&Subject { group: Group::Service, key: "X", display: "Vendor Firewall", path: "", reviewed: false, system_package: false }).is_some());

    for t in [
        r"\Microsoft\Windows\WindowsUpdate\Scheduled Start",
        r"\Microsoft\Windows\UpdateOrchestrator\Schedule Scan",
        r"\Microsoft\Windows\Windows Defender\Windows Defender Scheduled Scan",
        r"\Microsoft\Windows\Time Synchronization\SynchronizeTime",
        r"\Microsoft\Windows\Defrag\ScheduledDefrag",
        r"\Microsoft\Windows\TaskScheduler\Regular Maintenance",
        r"\Orbit Measure",
    ] {
        assert!(refused(Group::Task, t, ""), "task {t}");
    }
    // A reviewed Windows task is allowed; an unreviewed one is not.
    let ok = Subject { group: Group::Task, key: r"\Microsoft\Windows\Maps\MapsUpdateTask", display: "", path: "", reviewed: true, system_package: false };
    assert_eq!(guard::refuse(&ok), None);
    assert!(refused(Group::Task, r"\Microsoft\Windows\Maps\MapsUpdateTask", ""));

    for (name, path) in [
        ("SecurityHealth", r"C:\Windows\system32\SecurityHealthSystray.exe"),
        ("RtkAudUService", r"C:\Windows\System32\DriverStore\FileRepository\x\RtkAudUService64.exe"),
        ("WindowsThing", r"C:\Windows\System32\thing.exe"),
        ("Orbit", r"C:\Users\u\AppData\Local\Orbit\orbit.exe"),
    ] {
        assert!(refused(Group::Startup, name, path), "startup {name}");
    }

    for a in [
        "Microsoft.WindowsStore", "Microsoft.DesktopAppInstaller", "Microsoft.StorePurchaseApp", "Microsoft.VCLibs.140.00",
        "Microsoft.NET.Native.Framework.2.2", "Microsoft.UI.Xaml.2.8", "Microsoft.HEVCVideoExtension", "Microsoft.WebMediaExtensions",
        "Microsoft.SecHealthUI", "NVIDIACorp.NVIDIAControlPanel", "Microsoft.Windows.ShellExperienceHost",
    ] {
        assert!(refused(Group::App, a, ""), "app {a}");
    }
    let system = Subject { group: Group::App, key: "Microsoft.BingWeather", display: "", path: "", reviewed: true, system_package: true };
    assert!(guard::refuse(&system).is_some(), "system and framework packages");
}

#[test]
fn guard_wins_even_when_a_list_says_safe() {
    // A wrong list that calls Defender, the spooler and the Store safe.
    static BAD: &[Known] = &[
        Known { group: Group::Service, patterns: &["WinDefend", "Spooler"], name: "Bad", what: "x.", verdict: Verdict::Off, note: "x." },
        Known { group: Group::App, patterns: &["Microsoft.WindowsStore"], name: "Bad app", what: "x.", verdict: Verdict::Off, note: "x." },
        Known { group: Group::Startup, patterns: &["orbit.exe"], name: "Bad startup", what: "x.", verdict: Verdict::Off, note: "x." },
    ];
    let dir = temp_dir("bad");
    let f = fake(true, &dir);
    f.inv.borrow_mut().services.push(service("Spooler", "Print Spooler", "auto", r"C:\Windows\System32\spoolsv.exe", false));
    let a = analyze(BAD, &f.inventory(), true);
    for id in ["service:WinDefend", "service:Spooler", "app:Microsoft.WindowsStore_8wekyb3d8bbwe", r"startup:HKLM\Run\Orbit"] {
        let it = find(&a, id);
        assert_eq!(it.verdict, Verdict::Keep, "{id}");
        assert!(!it.actionable && it.guarded, "{id}");
    }
    let r = apply_with(&f, BAD, &dir, &ids(&["service:WinDefend", "service:Spooler", "app:Microsoft.WindowsStore_8wekyb3d8bbwe", r"startup:HKLM\Run\Orbit"])).unwrap();
    assert!(r.applied.is_empty());
    assert_eq!(r.refused.len(), 4);
    assert!(f.calls.borrow().is_empty(), "the guard let a call through: {:?}", f.calls.borrow());
}

#[test]
fn ui_cannot_ask_for_anything_but_safe_items() {
    let dir = temp_dir("ui");
    let f = fake(true, &dir);
    let asked = ids(&[
        "service:WinDefend",                                   // guarded
        "service:Audiosrv",                                    // guarded, not even listed
        "service:SysMain",                                     // Keep
        "service:VendorSvc",                                   // Unknown
        "service:TrkWks",                                      // Safe, already Manual
        r"task:\Microsoft\Windows\Defrag\ScheduledDefrag",     // unreviewed Windows task
        r"task:\MicrosoftEdgeUpdateTaskMachineCore",           // Keep
        r"task:\SomeVendor Helper",                            // Unknown
        r"startup:HKCU\Run\Spotify",                           // Safe, already off
        r"startup:HKCU\Run\Gadget",                            // Unknown (even if suggested)
        "app:Microsoft.WindowsCalculator_8wekyb3d8bbwe",       // not on the list
        "app:Microsoft.WindowsStore_8wekyb3d8bbwe",            // guarded
        "startup:HKCU\\Run\\Nope",                             // not on this PC
    ]);
    let p = preview_with(&f, KNOWN, &asked);
    assert!(p.changes.is_empty(), "{:?}", p.changes);
    assert_eq!(p.refused.len(), asked.len());
    let r = apply_with(&f, KNOWN, &dir, &asked).unwrap();
    assert!(r.applied.is_empty());
    assert!(f.calls.borrow().is_empty(), "{:?}", f.calls.borrow());
    assert!(load_log(&dir).unwrap().changes.is_empty());
}

// ---------------------------------------------------------------- analysis

#[test]
fn analysis_groups_and_explains() {
    let a = analyze(KNOWN, &sample(), false);
    let titles: Vec<&str> = a.groups.iter().map(|g| g.title.as_str()).collect();
    assert_eq!(titles, ["Startup apps", "Scheduled tasks", "Services", "Built-in apps"]);

    let steam = find(&a, r"startup:HKCU\Run\Steam");
    assert_eq!((steam.verdict, steam.actionable, steam.name.as_str()), (Verdict::Off, true, "Steam"));
    assert_eq!(steam.change.as_deref(), Some("Turn off at sign-in"));
    assert!(steam.note.as_deref().unwrap().contains("Steam opens"));

    let spotify = find(&a, r"startup:HKCU\Run\Spotify");
    assert_eq!((spotify.verdict, spotify.on, spotify.actionable), (Verdict::Off, false, false));

    let gadget = find(&a, r"startup:HKCU\Run\Gadget");
    assert_eq!((gadget.verdict, gadget.suggested, gadget.impact.as_deref()), (Verdict::Unknown, true, Some("high")));
    assert_eq!(gadget.search, "gadget.exe");

    assert_eq!(find(&a, r"startup:HKLM\Run\SecurityHealth").verdict, Verdict::Keep);

    let compat = find(&a, r"task:\Microsoft\Windows\Application Experience\Microsoft Compatibility Appraiser");
    assert_eq!((compat.verdict, compat.name.as_str(), compat.needs_admin), (Verdict::Off, "Windows compatibility check", true));
    let edge = find(&a, r"task:\MicrosoftEdgeUpdateTaskMachineCore");
    assert_eq!((edge.verdict, edge.name.as_str(), edge.raw.as_str()), (Verdict::Keep, "Microsoft Edge updater", r"\MicrosoftEdgeUpdateTaskMachineCore"));
    assert_eq!(find(&a, r"task:\SomeVendor Helper").verdict, Verdict::Unknown);
    let tasks: Vec<&str> = a.groups[1].items.iter().map(|i| i.raw.as_str()).collect();
    assert!(!tasks.iter().any(|t| t.contains("Defrag")), "unreviewed Windows tasks are not listed: {tasks:?}");

    let diag = find(&a, "service:DiagTrack");
    assert_eq!((diag.verdict, diag.name.as_str(), diag.detail.as_deref()), (Verdict::Off, "Windows telemetry", Some("Starts: Automatic")));
    assert_eq!(find(&a, "service:WinDefend").verdict, Verdict::Keep);
    assert_eq!(find(&a, "service:VendorSvc").verdict, Verdict::Unknown);
    assert_eq!(find(&a, "service:SysMain").verdict, Verdict::Keep);
    assert!(!a.groups[2].items.iter().any(|i| i.raw == "Audiosrv"), "Windows' own services are listed only when reviewed");
    let trk = find(&a, "service:TrkWks");
    assert_eq!((trk.verdict, trk.on), (Verdict::Off, false));

    let weather = find(&a, "app:Microsoft.BingWeather_8wekyb3d8bbwe");
    assert_eq!((weather.verdict, weather.change.as_deref()), (Verdict::Off, Some("Remove for this user")));
    assert_eq!(weather.family.as_deref(), Some("Microsoft.BingWeather_8wekyb3d8bbwe"));
    assert_eq!(find(&a, "app:Microsoft.WindowsStore_8wekyb3d8bbwe").verdict, Verdict::Keep);
    assert!(!a.groups[3].items.iter().any(|i| i.raw == "Microsoft.WindowsCalculator"));

    // Safe first, then Keep, then Unknown.
    let order: Vec<Verdict> = a.groups[0].items.iter().map(|i| i.verdict).collect();
    let mut sorted = order.clone();
    sorted.sort_by_key(|v| match v {
        Verdict::Off => 0,
        Verdict::Keep => 1,
        Verdict::Unknown => 2,
    });
    assert_eq!(order, sorted);

    // Steam, compat check, Zoom updater, DiagTrack, MapsBroker, Weather, Clipchamp.
    assert_eq!(a.counts.ready, 7);
    assert_eq!(a.groups.iter().map(|g| g.counts.ready).sum::<u32>(), a.counts.ready);
}

#[test]
fn preview_lists_exactly_what_changes() {
    let dir = temp_dir("preview");
    let f = fake(false, &dir);
    let p = preview_with(&f, KNOWN, &ids(&[r"startup:HKCU\Run\Steam", "service:MapsBroker", "app:Clipchamp.Clipchamp_yxz26nhyzhsrt"]));
    assert_eq!(p.changes.len(), 2);
    assert_eq!(p.changes[0].action, "Turn off at sign-in");
    assert_eq!((p.changes[0].from.as_str(), p.changes[0].to.as_str()), ("On at sign-in", "Off"));
    assert_eq!((p.changes[1].from.as_str(), p.changes[1].to.as_str()), ("Installed", "Removed for you"));
    assert_eq!(p.refused.len(), 1);
    assert!(p.refused[0].needs_admin, "services need admin");
    assert!(f.calls.borrow().is_empty(), "a preview changes nothing");
    assert!(!dir.exists(), "a preview writes nothing");
}

// ---------------------------------------------------------------- apply and undo

#[test]
fn every_change_is_logged_before_it_is_made() {
    let dir = temp_dir("apply");
    let f = fake(true, &dir);
    let all = ids(&[
        r"startup:HKCU\Run\Steam",
        r"task:\Microsoft\Windows\Application Experience\Microsoft Compatibility Appraiser",
        "service:MapsBroker",
        "app:Microsoft.BingWeather_8wekyb3d8bbwe",
    ]);
    let r = apply_with(&f, KNOWN, &dir, &all).unwrap();
    assert_eq!(r.applied.len(), 4, "{:?} {:?}", r.failed, r.refused);
    assert_eq!(
        *f.calls.borrow(),
        vec![
            r"startup HKCU\Run\Steam false".to_string(),
            r"task \Microsoft\Windows\Application Experience\Microsoft Compatibility Appraiser false".into(),
            "service MapsBroker manual".into(),
            "remove Microsoft.BingWeather_1.0.0.0_x64__8wekyb3d8bbwe".into(),
        ]
    );
    assert_eq!(f.pending_seen.borrow().len(), 4, "each change was on disk before the call");
    let log = load_log(&dir).unwrap();
    assert!(log.changes.iter().all(|c| c.state == ChangeState::Applied));
    let maps = log.changes.iter().find(|c| c.target == "MapsBroker").unwrap();
    assert_eq!((maps.before.as_str(), maps.from.as_str()), ("autoDelayed", "Automatic (delayed)"));
    let weather = log.changes.iter().find(|c| c.group == Group::App).unwrap();
    assert_eq!(weather.store_url.as_deref(), Some("ms-windows-store://pdp/?PFN=Microsoft.BingWeather_8wekyb3d8bbwe"));
    // Applying again changes nothing: they are already off.
    let again = apply_with(&f, KNOWN, &dir, &all).unwrap();
    assert!(again.applied.is_empty());
    assert_eq!(f.calls.borrow().len(), 4);
}

#[test]
fn a_failed_change_leaves_nothing_to_undo() {
    let dir = temp_dir("fail");
    let mut f = fake(true, &dir);
    f.fail = vec!["service MapsBroker".into()];
    let r = apply_with(&f, KNOWN, &dir, &ids(&["service:MapsBroker", r"startup:HKCU\Run\Steam"])).unwrap();
    assert_eq!(r.failed.len(), 1);
    assert!(r.failed[0].error.contains("administrator"));
    assert_eq!(r.applied.len(), 1);
    let log = load_log(&dir).unwrap();
    assert_eq!(log.changes.len(), 1);
    assert_eq!(log.changes[0].target, r"HKCU\Run\Steam");
}

#[test]
fn undo_restores_the_previous_state() {
    let dir = temp_dir("undo");
    let f = fake(true, &dir);
    let r = apply_with(&f, KNOWN, &dir, &ids(&[
        r"startup:HKCU\Run\Steam",
        r"task:\ZoomUpdateTaskUser-S-1-5-21-1",
        "service:MapsBroker",
        "service:DiagTrack",
        "app:Clipchamp.Clipchamp_yxz26nhyzhsrt",
    ]))
    .unwrap();
    assert_eq!(r.applied.len(), 5);
    f.calls.borrow_mut().clear();
    let id_of = |target: &str| r.applied.iter().find(|c| c.target.contains(target)).unwrap().id;

    let c = undo_with(&f, &dir, id_of("MapsBroker")).unwrap();
    assert_eq!(c.state, ChangeState::Undone);
    assert_eq!(f.calls.borrow().last().unwrap(), "service MapsBroker autoDelayed");
    assert_eq!(f.inventory().services.iter().find(|s| s.name == "MapsBroker").unwrap().start_type, "autoDelayed");
    assert!(undo_with(&f, &dir, id_of("MapsBroker")).unwrap_err().contains("already undone"));

    // A removed app: undo opens its Store page; it counts as undone once it is back.
    let app = undo_with(&f, &dir, id_of("Clipchamp")).unwrap();
    assert_eq!(app.state, ChangeState::Applied);
    assert_eq!(f.calls.borrow().last().unwrap(), "store Clipchamp.Clipchamp_yxz26nhyzhsrt");
    f.installed.borrow_mut().push("Clipchamp.Clipchamp_yxz26nhyzhsrt".into());
    let listed = changes_with(&f, &dir).unwrap();
    assert_eq!(listed.iter().find(|c| c.group == Group::App).unwrap().state, ChangeState::Undone);

    // Undo all: the rest, newest first.
    let all = undo_all_with(&f, &dir).unwrap();
    assert_eq!(all.undone.len(), 3);
    assert!(all.store_apps.is_empty());
    let calls = f.calls.borrow();
    let tail: Vec<&str> = calls.iter().rev().take(3).map(String::as_str).collect();
    assert_eq!(tail, [r"startup HKCU\Run\Steam true", r"task \ZoomUpdateTaskUser-S-1-5-21-1 true", "service DiagTrack auto"]);
    let inv = f.inventory();
    assert!(inv.startup.iter().find(|e| e.name == "Steam").unwrap().enabled);
    assert!(inv.tasks.iter().find(|t| t.path.contains("Zoom")).unwrap().enabled);
    assert_eq!(inv.services.iter().find(|s| s.name == "DiagTrack").unwrap().start_type, "auto");
    assert!(load_log(&dir).unwrap().changes.iter().all(|c| c.state == ChangeState::Undone));
}

#[test]
fn undo_all_leaves_removed_apps_to_the_store() {
    let dir = temp_dir("undoall");
    let f = fake(false, &dir);
    apply_with(&f, KNOWN, &dir, &ids(&["app:Microsoft.BingWeather_8wekyb3d8bbwe", r"startup:HKCU\Run\Steam"])).unwrap();
    let all = undo_all_with(&f, &dir).unwrap();
    assert_eq!(all.undone.len(), 1);
    assert_eq!(all.store_apps.len(), 1);
    assert!(!f.calls.borrow().iter().any(|c| c.starts_with("store")), "undo all opens no Store pages");
}

#[test]
fn undo_of_an_admin_change_needs_admin() {
    let dir = temp_dir("admin");
    let f = fake(true, &dir);
    let r = apply_with(&f, KNOWN, &dir, &ids(&["service:DiagTrack"])).unwrap();
    let user = Fake { inv: RefCell::new(f.inventory()), admin: false, ..Default::default() };
    let err = undo_with(&user, &dir, r.applied[0].id).unwrap_err();
    assert!(err.contains("administrator"), "{err}");
    assert!(user.calls.borrow().is_empty());
    // Not admin: services are refused before anything is written.
    let dir2 = temp_dir("admin2");
    let g = fake(false, &dir2);
    let r2 = apply_with(&g, KNOWN, &dir2, &ids(&["service:DiagTrack"])).unwrap();
    assert!(r2.applied.is_empty() && r2.refused[0].needs_admin);
}

#[test]
fn a_pending_change_from_a_crash_can_be_undone() {
    let dir = temp_dir("crash");
    let log = ChangeLog {
        version: 1,
        next_id: 2,
        changes: vec![Change {
            id: 1,
            item: r"startup:HKCU\Run\Steam".into(),
            group: Group::Startup,
            name: "Steam".into(),
            raw: "Steam".into(),
            target: r"HKCU\Run\Steam".into(),
            action: "Turn off at sign-in".into(),
            from: "On at sign-in".into(),
            to: "Off".into(),
            before: "enabled".into(),
            family: None,
            store_url: None,
            needs_admin: false,
            at: 1,
            state: ChangeState::Pending,
            undone_at: None,
        }],
    };
    save_log(&dir, &log).unwrap();
    let f = Fake { inv: RefCell::new(sample()), ..Default::default() };
    assert_eq!(undo_all_with(&f, &dir).unwrap().undone.len(), 1);
    assert_eq!(*f.calls.borrow(), vec![r"startup HKCU\Run\Steam true".to_string()]);
}

#[test]
fn store_family_names_are_checked() {
    assert!(valid_family("Microsoft.BingWeather_8wekyb3d8bbwe"));
    assert!(valid_family("king.com.CandyCrushSaga_kgqvnymyfvs32"));
    for bad in ["", "NoUnderscore", "a_b_c", "_x", "x_", "x_y&z", "x_y/../z", "ms-windows-store://x_y"] {
        assert!(!valid_family(bad), "{bad}");
    }
}

// ---------------------------------------------------------------- this PC (read only)

/// Reads this PC and prints what Debloat would show. Changes nothing.
/// `cargo test debloat::tests::real_analysis -- --ignored --nocapture`
#[test]
#[ignore]
fn real_analysis() {
    let b = system::Real;
    let inv = b.inventory();
    let a = analyze(KNOWN, &inv, b.is_admin());
    println!("admin: {}  errors: {:?}", a.is_admin, a.errors);
    println!("total: {:?}", a.counts);
    for g in &a.groups {
        println!("\n== {} {:?}", g.title, g.counts);
        for it in &g.items {
            let v = match it.verdict {
                Verdict::Off => if it.actionable { "SAFE" } else if it.guarded { "safe/guarded" } else { "safe/already off" },
                Verdict::Keep => "keep",
                Verdict::Unknown => "unknown",
            };
            if it.verdict != Verdict::Unknown || it.suggested {
                println!("  [{v}] {} ({}){}{}", it.name, it.raw, if it.needs_admin { " admin" } else { "" }, if it.suggested { " suggested" } else { "" });
            }
        }
        let unknown = g.items.iter().filter(|i| i.verdict == Verdict::Unknown).count();
        println!("  + {unknown} unknown");
    }
    let _ = known::lookup(KNOWN, Group::App, &["x"]);
}
