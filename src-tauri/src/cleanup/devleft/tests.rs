//! Developer leftovers tests: throwaway folders under the cargo target dir
//! and `Layout::under` only, never real projects.

use super::*;
use std::os::windows::fs::OpenOptionsExt;
use std::process::Command;

fn base(name: &str) -> PathBuf {
    let target = std::env::var("CARGO_TARGET_DIR")
        .map(|t| PathBuf::from(t.replace('/', "\\")))
        .unwrap_or_else(|_| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target"));
    let b = target.join("devleft-tests").join(format!("{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&b);
    fs::create_dir_all(&b).unwrap();
    b
}

fn put(p: &Path) {
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, b"xxxx").unwrap();
}

/// Sets the modified (or only the accessed) time of a file or folder.
fn touch(p: &Path, at: SystemTime, accessed_only: bool) {
    let f = fs::OpenOptions::new()
        .access_mode(0x100) // FILE_WRITE_ATTRIBUTES
        .custom_flags(0x0200_0000) // FILE_FLAG_BACKUP_SEMANTICS, so folders open too
        .open(p)
        .unwrap();
    let t = if accessed_only { fs::FileTimes::new().set_accessed(at) } else { fs::FileTimes::new().set_modified(at) };
    f.set_times(t).unwrap();
}

fn junction(link: &Path, target: &Path) {
    let ok = Command::new("cmd").args(["/C", "mklink", "/J"]).arg(link).arg(target).output().unwrap().status.success();
    assert!(ok, "mklink /J failed");
}

/// "Now" 60 days ahead, so everything just made counts as untouched.
fn later() -> SystemTime {
    SystemTime::now() + Duration::from_secs(60 * DAY)
}

/// Ten days before `later()`: inside the 30-day window.
fn recent() -> SystemTime {
    later() - Duration::from_secs(10 * DAY)
}

fn node(dir: &Path) {
    put(&dir.join("package.json"));
    put(&dir.join(r"node_modules\pkg\index.js"));
    put(&dir.join(r"node_modules\pkg\deep\x.js"));
}

fn rust(dir: &Path) {
    put(&dir.join("Cargo.toml"));
    put(&dir.join(r"target\debug\app.exe"));
}

fn setup(name: &str) -> (PathBuf, Layout, PathBuf) {
    let b = base(name);
    let layout = Layout::under(&b);
    let work = b.join("Work");
    fs::create_dir_all(&work).unwrap();
    add_folder(&layout, &work.to_string_lossy()).unwrap();
    (b, layout, work)
}

fn listed(layout: &Layout, now: SystemTime) -> (Vec<String>, DevScan, Vec<Found>) {
    let (s, found) = scan(layout, &load(layout), now);
    let mut paths: Vec<String> = s.projects.iter().map(|p| p.path.to_lowercase()).collect();
    paths.sort();
    (paths, s, found)
}

fn low(p: PathBuf) -> String {
    p.to_string_lossy().to_lowercase()
}

fn hold_ids(layout: &Layout, found: &[Found]) -> DevResult {
    let ids: Vec<String> = found.iter().map(|f| f.id.clone()).collect();
    hold_all(layout, &resolve(found, &ids).unwrap(), later())
}

#[test]
fn manifest_pairing() {
    let (_b, layout, w) = setup("pairing");
    node(&w.join("a"));
    put(&w.join(r"b\node_modules\pkg\index.js")); // no package.json
    rust(&w.join("c"));
    put(&w.join(r"d\target\debug\x")); // no Cargo.toml
    put(&w.join(r"e\package.json")); // wrong manifest for target
    put(&w.join(r"e\target\x"));
    let (paths, s, _) = listed(&layout, later());
    assert_eq!(paths, [low(w.join(r"a\node_modules")), low(w.join(r"c\target"))]);
    let a = s.projects.iter().find(|p| p.kind == Kind::Node).unwrap();
    assert_eq!((a.name.as_str(), a.files, a.bytes), ("a", 2, 8));
}

#[test]
fn outermost_only() {
    let (_b, layout, w) = setup("outermost");
    node(&w.join("a"));
    node(&w.join(r"a\node_modules\pkg")); // nested project inside node_modules
    rust(&w.join("r"));
    rust(&w.join(r"r\target\sub")); // a crate inside a matched target
    node(&w.join(r"loose\node_modules\proj")); // node_modules with no manifest: never entered
    let (paths, ..) = listed(&layout, later());
    assert_eq!(paths, [low(w.join(r"a\node_modules")), low(w.join(r"r\target"))]);
}

#[test]
fn protected_roots() {
    let (b, mut layout, w) = setup("protected");
    let drive = path_parts(&b.to_string_lossy()).unwrap()[0].to_uppercase() + "\\";
    assert_eq!(folder_problem(&layout, &drive), Some("Pick a folder, not a whole drive."));
    assert!(folder_problem(&layout, &b.to_string_lossy()).is_some(), "the test root holds protected folders");
    for p in [b.join(r"ProgramFiles\App"), b.join(r"User\AppData\Local\Programs\code"), b.join(r"Windows\x")] {
        node(&p);
        assert!(folder_problem(&layout, &p.to_string_lossy()).is_some(), "{p:?}");
        assert!(add_folder(&layout, &p.to_string_lossy()).is_err());
    }
    for real in [r"C:\Program Files\nodejs", r"C:\Program Files (x86)\x", r"C:\Windows\System32"] {
        assert!(path_problem(&layout, real).is_some(), "{real}");
    }
    // A link is never a projects folder.
    fs::create_dir_all(b.join("Real")).unwrap();
    junction(&w.join("link"), &b.join("Real"));
    assert!(folder_problem(&layout, &w.join("link").to_string_lossy()).is_some());
    assert_eq!(load(&layout).folders.len(), 1);
    // The walk skips no-go folders inside a picked one.
    node(&w.join(r"apps\tool"));
    node(&w.join("mine"));
    layout.nogo.push(w.join("apps").to_string_lossy().into_owned());
    let (paths, ..) = listed(&layout, later());
    assert_eq!(paths, [low(w.join(r"mine\node_modules"))]);
}

#[test]
fn age_rule() {
    let (_b, layout, w) = setup("age");
    for p in ["a", "b", "c", "d", "e", "f", "g"] {
        node(&w.join(p));
    }
    rust(&w.join("h"));
    put(&w.join(r"e\package-lock.json"));
    put(&w.join(r"h\Cargo.lock"));
    touch(&w.join(r"b\node_modules\pkg"), recent(), false); // a direct child of the build folder
    touch(&w.join("c"), recent(), false); // the project folder
    touch(&w.join(r"d\package.json"), recent(), false);
    touch(&w.join(r"e\package-lock.json"), recent(), false);
    touch(&w.join(r"f\node_modules\pkg\deep\x.js"), recent(), false); // deeper: not looked at
    touch(&w.join(r"g\package.json"), recent(), true); // access time: not used
    touch(&w.join(r"h\Cargo.lock"), recent(), false);
    let (paths, s, _) = listed(&layout, later());
    assert_eq!(paths, [low(w.join(r"a\node_modules")), low(w.join(r"f\node_modules")), low(w.join(r"g\node_modules"))]);
    assert_eq!(s.recent, 5);
    // With the real clock nothing qualifies: everything was just made.
    let (none, s, _) = listed(&layout, SystemTime::now());
    assert!(none.is_empty());
    assert_eq!(s.recent, 8);
}

#[test]
fn id_resolution() {
    let (_b, layout, w) = setup("ids");
    node(&w.join("a"));
    let (_, s, found) = listed(&layout, later());
    let id = s.projects[0].id.clone();
    assert!(resolve(&found, &["nope".to_string()]).is_err());
    assert!(resolve(&found, &[]).is_err());
    let r = resolve(&found, &[id.clone(), id.clone()]).unwrap();
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].build, w.join(r"a\node_modules"));
    // A new scan makes new ids.
    let (_, s2, found2) = listed(&layout, later());
    assert_ne!(s2.projects[0].id, id);
    assert!(resolve(&found2, &[id]).is_err());

    let f = &found[0];
    assert!(recheck(&layout, &load(&layout), f, later()).is_ok());
    remove_folder(&layout, &w.to_string_lossy()).unwrap();
    assert!(recheck(&layout, &load(&layout), f, later()).is_err());
    add_folder(&layout, &w.to_string_lossy()).unwrap();
    fs::remove_file(w.join(r"a\package.json")).unwrap();
    assert!(recheck(&layout, &load(&layout), f, later()).is_err());
    put(&w.join(r"a\package.json"));
    touch(&w.join(r"a\package.json"), recent(), false);
    assert!(recheck(&layout, &load(&layout), f, later()).is_err());
    // A path the scan never found can't be smuggled in by id.
    let forged = Found { build: w.join(r"a\src"), ..f.clone() };
    assert!(recheck(&layout, &load(&layout), &forged, later()).is_err());
}

#[test]
fn hold_restore_and_expiry() {
    let (b, layout, w) = setup("hold");
    node(&w.join("a"));
    let hold = b.join(r"OrbitData\hold");
    let (_, _, found) = listed(&layout, later());
    let r = hold_ids(&layout, &found);
    assert_eq!((r.done, r.bytes, r.kept.len()), (1, 8, 0));
    assert!(!w.join(r"a\node_modules").exists());
    assert_eq!(fs::read_dir(&hold).unwrap().count(), 1);
    let st = state(&layout);
    assert_eq!(st.held.len(), 1);
    assert_eq!(st.held[0].expires_at - st.held[0].held_at, HOLD_DAYS * DAY);
    assert!(same_path(&st.held[0].original, &w.join(r"a\node_modules").to_string_lossy()));

    let r = act_on_held(&layout, &[st.held[0].id.clone()], true).unwrap();
    assert_eq!((r.done, r.kept.len()), (1, 0));
    assert!(w.join(r"a\node_modules\pkg\deep\x.js").is_file());
    assert!(state(&layout).held.is_empty());

    let (_, _, found) = listed(&layout, later());
    assert_eq!(hold_ids(&layout, &found).done, 1);
    assert_eq!(expire(&layout, later() + Duration::from_secs(6 * DAY)).done, 0);
    assert_eq!(state(&layout).held.len(), 1);
    let r = expire(&layout, later() + Duration::from_secs(HOLD_DAYS * DAY));
    assert_eq!((r.done, r.bytes), (1, 8));
    assert!(state(&layout).held.is_empty());
    assert_eq!(fs::read_dir(&hold).unwrap().count(), 0);
    assert!(w.join(r"a\package.json").is_file());
}

#[test]
fn refuses_paths_outside_the_hold() {
    let (b, layout, w) = setup("outside");
    let hold = b.join(r"OrbitData\hold");
    owned_hold(&hold, HOLD_SDDL);
    put(&b.join(r"Outside\node_modules\keep.txt"));
    put(&w.join(r"a\node_modules\keep.txt"));
    junction(&hold.join("link"), &b.join(r"Outside\node_modules"));
    let original = w.join(r"a\node_modules").to_string_lossy().into_owned();
    let rec = |id: &str, hold_name: &str| Held { id: id.into(), original: original.clone(), hold_name: hold_name.into(), held_at: 0, bytes: 1, pending: false };
    let mut s = load(&layout);
    s.held = vec![
        rec("up", r"..\..\Outside\node_modules"),
        rec("abs", &b.join(r"Outside\node_modules").to_string_lossy()),
        rec("link", "link"),
        rec("missing", "missing"),
    ];
    save(&layout, &s).unwrap();
    let ids: Vec<String> = ["up", "abs", "link", "missing"].iter().map(|s| s.to_string()).collect();
    let r = act_on_held(&layout, &ids, false).unwrap();
    assert_eq!((r.done, r.kept.len()), (0, 4));
    let r = expire(&layout, later());
    assert_eq!(r.done, 0);
    assert!(b.join(r"Outside\node_modules\keep.txt").is_file());
    assert!(w.join(r"a\node_modules\keep.txt").is_file());
    // Restore can't pull them in either.
    fs::remove_dir_all(w.join(r"a\node_modules")).unwrap();
    let left: Vec<String> = load(&layout).held.iter().map(|h| h.id.clone()).collect();
    let r = act_on_held(&layout, &left, true).unwrap();
    assert_eq!(r.done, 0);
    assert!(!w.join(r"a\node_modules").exists());
    assert!(b.join(r"Outside\node_modules\keep.txt").is_file());
}

#[test]
fn restore_conflict_and_place() {
    let (b, layout, w) = setup("conflict");
    node(&w.join("a"));
    let (_, _, found) = listed(&layout, later());
    assert_eq!(hold_ids(&layout, &found).done, 1);
    let id = state(&layout).held[0].id.clone();
    put(&w.join(r"a\node_modules\new.txt"));
    let r = act_on_held(&layout, &[id.clone()], true).unwrap();
    assert_eq!(r.done, 0);
    assert!(r.kept[0].reason.contains("already at"), "{:?}", r.kept);
    assert!(w.join(r"a\node_modules\new.txt").is_file());
    assert!(!w.join(r"a\node_modules\pkg").exists());
    assert_eq!(state(&layout).held.len(), 1);

    // Only back inside a picked folder: an edited original is refused.
    fs::remove_dir_all(w.join(r"a\node_modules")).unwrap();
    let mut s = load(&layout);
    let elsewhere = b.join(r"Elsewhere\p");
    fs::create_dir_all(&elsewhere).unwrap();
    s.held[0].original = elsewhere.join("node_modules").to_string_lossy().into_owned();
    save(&layout, &s).unwrap();
    let r = act_on_held(&layout, &[id.clone()], true).unwrap();
    assert!(r.kept[0].reason.contains("only puts folders back"), "{:?}", r.kept);
    assert!(!elsewhere.join("node_modules").exists());
    // ...and with the projects folder removed from the list.
    s.held[0].original = w.join(r"a\node_modules").to_string_lossy().into_owned();
    save(&layout, &s).unwrap();
    remove_folder(&layout, &w.to_string_lossy()).unwrap();
    let r = act_on_held(&layout, &[id.clone()], true).unwrap();
    assert_eq!(r.done, 0);
    add_folder(&layout, &w.to_string_lossy()).unwrap();
    let r = act_on_held(&layout, &[id], true).unwrap();
    assert_eq!(r.done, 1);
    assert!(w.join(r"a\node_modules\pkg\index.js").is_file());
}

#[test]
fn unusable_hold_keeps_in_place() {
    let (b, layout, w) = setup("nohold");
    node(&w.join("a"));
    put(&b.join(r"OrbitData\hold")); // a file where the hold folder should be
    let (_, _, found) = listed(&layout, later());
    let r = hold_ids(&layout, &found);
    assert_eq!((r.done, r.kept.len()), (0, 1));
    assert!(w.join(r"a\node_modules\pkg\index.js").is_file());
    assert!(state(&layout).held.is_empty());
}

#[test]
fn links_are_never_moved() {
    let (b, layout, w) = setup("links");
    put(&w.join(r"j\package.json"));
    put(&b.join(r"Outside\keep.txt"));
    junction(&w.join(r"j\node_modules"), &b.join("Outside"));
    let (paths, ..) = listed(&layout, later());
    assert!(paths.is_empty());
    let forged = Found {
        id: "x".into(),
        root: w.clone(),
        project: w.join("j"),
        build: w.join(r"j\node_modules"),
        kind: Kind::Node,
        bytes: 0,
    };
    let r = hold_all(&layout, &[&forged], later());
    assert_eq!(r.done, 0);
    assert!(b.join(r"Outside\keep.txt").is_file());
}

#[test]
fn hold_folder_per_drive() {
    let b = base("drives");
    let layout = Layout::under(&b);
    let drive = path_parts(&b.to_string_lossy()).unwrap()[0].clone();
    assert_eq!(hold_dir_for(&layout, &drive), Some(b.join(r"OrbitData\hold")));
    let other = if drive == "q:" { "r:" } else { "q:" };
    assert_eq!(hold_dir_for(&layout, other), Some(PathBuf::from(format!(r"{}\.orbit-hold", other.to_uppercase()))));
    assert_eq!(hold_dir_for(&layout, r"\\srv\share"), None);
    assert!(!plain_name(r"..\x") && !plain_name("a/b") && !plain_name("..") && plain_name("1-2-target"));
}

fn owned_hold(hold: &Path, sddl: &str) {
    fs::create_dir_all(hold.parent().unwrap()).unwrap();
    create_with_sddl(hold, sddl).unwrap();
}

/// Owner-only, plus Authenticated Users with modify rights.
const OPEN_SDDL: &str = "D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;FA;;;OW)(A;OICI;0x1301bf;;;AU)";

#[test]
fn acl_rules() {
    let me = "S-1-5-21-1-2-3-1001";
    let acl = |owner: &str, w: Option<&[&str]>| HoldAcl { owner: owner.into(), writers: w.map(|v| v.iter().map(|s| s.to_string()).collect()) };
    assert!(acl_trusted(&acl(me, Some(&["S-1-5-18", "S-1-5-32-544", "S-1-3-4", me])), me));
    assert!(acl_trusted(&acl("S-1-5-32-544", Some(&["S-1-5-18"])), me));
    // Owned by another user.
    assert!(!acl_trusted(&acl("S-1-5-21-1-2-3-1002", Some(&["S-1-5-18"])), me));
    // Others can write: Users, Authenticated Users, Everyone, another user, unknown entry types.
    for w in ["S-1-5-32-545", "S-1-5-11", "S-1-1-0", "S-1-5-21-1-2-3-1002", "?"] {
        assert!(!acl_trusted(&acl(me, Some(&["S-1-5-18", w])), me), "{w}");
    }
    // No DACL at all lets everyone in.
    assert!(!acl_trusted(&acl(me, None), me));
}

#[test]
fn real_hold_acl_is_checked() {
    let (b, layout, w) = setup("acl");
    // The hold Orbit makes passes its own check.
    node(&w.join("a"));
    let (_, _, found) = listed(&layout, later());
    assert_eq!(hold_ids(&layout, &found).done, 1);
    let held = state(&layout).held[0].id.clone();
    assert_eq!(act_on_held(&layout, &[held], true).unwrap().done, 1);
    // A hold others can write to is refused for holding, restore and delete.
    let (b2, layout2, w2) = setup("acl-open");
    let hold = b2.join(r"OrbitData\hold");
    owned_hold(&hold, OPEN_SDDL);
    node(&w2.join("a"));
    let (_, _, found) = listed(&layout2, later());
    let r = hold_ids(&layout2, &found);
    assert_eq!((r.done, r.kept.len()), (0, 1));
    assert!(r.kept[0].reason.contains("other users"), "{:?}", r.kept);
    assert!(w2.join(r"a\node_modules\pkg\index.js").is_file());
    put(&hold.join(r"1-0-node_modules\planted.js"));
    let mut s = load(&layout2);
    s.held.push(Held { id: "p".into(), original: w2.join(r"b\node_modules").to_string_lossy().into_owned(), hold_name: "1-0-node_modules".into(), held_at: 1, bytes: 1, pending: false });
    save(&layout2, &s).unwrap();
    fs::create_dir_all(w2.join("b")).unwrap();
    assert_eq!(act_on_held(&layout2, &["p".to_string()], true).unwrap().done, 0);
    assert_eq!(act_on_held(&layout2, &["p".to_string()], false).unwrap().done, 0);
    assert_eq!(expire(&layout2, later()).done, 0);
    assert!(hold.join(r"1-0-node_modules\planted.js").is_file());
    assert!(!w2.join(r"b\node_modules").exists());
    let _ = b;
}

#[test]
fn failed_save_leaves_it_in_place() {
    let (b, layout, w) = setup("savefail");
    node(&w.join("a"));
    let (_, _, found) = listed(&layout, later());
    fs::create_dir_all(b.join(r"OrbitData\dev-leftovers.tmp")).unwrap(); // save can't write its temp file
    let r = hold_ids(&layout, &found);
    assert_eq!((r.done, r.kept.len()), (0, 1));
    assert!(same_path(&r.kept[0].path, &w.join(r"a\node_modules").to_string_lossy()));
    assert!(w.join(r"a\node_modules\pkg\index.js").is_file());
    let hold = b.join(r"OrbitData\hold");
    assert_eq!(fs::read_dir(&hold).map(|d| d.count()).unwrap_or(0), 0);
}

#[test]
fn crash_leftovers_are_settled_and_expired() {
    let (b, layout, w) = setup("crash");
    node(&w.join("a"));
    let (_, _, found) = listed(&layout, later());
    assert_eq!(hold_ids(&layout, &found).done, 1);
    let hold = b.join(r"OrbitData\hold");
    // As if Orbit stopped between the move and the confirm, plus a record whose move never ran.
    let mut s = load(&layout);
    s.held[0].pending = true;
    s.held.push(Held { id: "never".into(), original: w.join(r"x\node_modules").to_string_lossy().into_owned(), hold_name: "9-9-node_modules".into(), held_at: 9, bytes: 1, pending: true });
    save(&layout, &s).unwrap();
    // Unlisted folders in the hold: one held long ago, one just now.
    put(&hold.join(r"1000-0-node_modules\old.js"));
    let fresh = format!("{}-0-target", later().duration_since(UNIX_EPOCH).unwrap().as_secs());
    put(&hold.join(&fresh).join("new.rs"));
    let r = expire(&layout, later());
    assert_eq!(r.done, 1, "{:?}", r.kept);
    assert!(!hold.join("1000-0-node_modules").exists());
    assert!(hold.join(&fresh).is_dir());
    let left = load(&layout).held;
    assert_eq!(left.len(), 1);
    assert!(!left[0].pending);
    assert_eq!(state(&layout).held.len(), 1);
}

#[test]
fn batches_with_a_bad_id() {
    let (_b, layout, w) = setup("batch");
    for p in ["a", "b", "c"] {
        node(&w.join(p));
    }
    let (_, _, found) = listed(&layout, later());
    assert_eq!(hold_ids(&layout, &found).done, 3);
    let ids: Vec<String> = state(&layout).held.iter().map(|h| h.id.clone()).collect();
    let batch = vec![ids[0].clone(), "bogus".to_string(), ids[1].clone(), ids[2].clone()];
    let r = act_on_held(&layout, &batch, true).unwrap();
    assert_eq!((r.done, r.kept.len()), (3, 1));
    assert!(state(&layout).held.is_empty(), "the batch was saved");
    for p in ["a", "b", "c"] {
        assert!(w.join(p).join(r"node_modules\pkg\index.js").is_file());
    }
}
