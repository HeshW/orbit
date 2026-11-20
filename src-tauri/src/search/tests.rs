//! File search unit tests: byte buffers and in-memory indexes only, never a
//! live volume.

use super::index::*;

/// A USN_RECORD_V2 for `name`.
fn v2(file: u64, parent: u64, usn: i64, reason: u32, attrs: u32, name: &str) -> Vec<u8> {
    let units: Vec<u16> = name.encode_utf16().collect();
    let len = (60 + units.len() * 2 + 7) / 8 * 8;
    let mut b = vec![0u8; len];
    b[0..4].copy_from_slice(&(len as u32).to_le_bytes());
    b[4..6].copy_from_slice(&2u16.to_le_bytes());
    b[8..16].copy_from_slice(&file.to_le_bytes());
    b[16..24].copy_from_slice(&parent.to_le_bytes());
    b[24..32].copy_from_slice(&usn.to_le_bytes());
    b[40..44].copy_from_slice(&reason.to_le_bytes());
    b[52..56].copy_from_slice(&attrs.to_le_bytes());
    b[56..58].copy_from_slice(&((units.len() * 2) as u16).to_le_bytes());
    b[58..60].copy_from_slice(&60u16.to_le_bytes());
    for (i, u) in units.iter().enumerate() {
        b[60 + i * 2..62 + i * 2].copy_from_slice(&u.to_le_bytes());
    }
    b
}

/// A USN_RECORD_V3 (128-bit references).
fn v3(file: u64, parent: u64, name: &str) -> Vec<u8> {
    let units: Vec<u16> = name.encode_utf16().collect();
    let len = (76 + units.len() * 2 + 7) / 8 * 8;
    let mut b = vec![0u8; len];
    b[0..4].copy_from_slice(&(len as u32).to_le_bytes());
    b[4..6].copy_from_slice(&3u16.to_le_bytes());
    b[8..16].copy_from_slice(&file.to_le_bytes());
    b[24..32].copy_from_slice(&parent.to_le_bytes());
    b[72..74].copy_from_slice(&((units.len() * 2) as u16).to_le_bytes());
    b[74..76].copy_from_slice(&76u16.to_le_bytes());
    for (i, u) in units.iter().enumerate() {
        b[76 + i * 2..78 + i * 2].copy_from_slice(&u.to_le_bytes());
    }
    b
}

fn reply(next: u64, recs: &[Vec<u8>]) -> Vec<u8> {
    let mut b = next.to_le_bytes().to_vec();
    for r in recs {
        b.extend_from_slice(r);
    }
    b
}

const SEQ: u64 = 3 << 48; // a sequence number in the high bits must be ignored

fn rec(file: u32, parent: u32, reason: u32, dir: bool, name: &str) -> Record {
    Record { file: SEQ | u64::from(file), parent: SEQ | u64::from(parent), usn: 0, reason, attrs: if dir { 0x10 } else { 0x20 }, name: name.into() }
}

// ---------------------------------------------------------------- parsing

#[test]
fn parses_v2_and_v3_records() {
    let buf = reply(77, &[v2(SEQ | 40, 5, 9, 0x100, 0x10, "Users"), v3(41, 40, "notes.txt")]);
    assert_eq!(reply_next(&buf), Some(77));
    let r = parse_records(&buf);
    assert_eq!(r.len(), 2);
    assert_eq!((record_of(r[0].file), r[0].parent, r[0].usn, r[0].reason, r[0].name.as_str()), (Some(40), 5, 9, 0x100, "Users"));
    assert_eq!((r[1].file, r[1].parent, r[1].name.as_str()), (41, 40, "notes.txt"));
}

#[test]
fn zero_length_record_stops() {
    let mut buf = reply(0, &[v2(30, 5, 0, 0, 0, "a.txt")]);
    buf.extend_from_slice(&[0u8; 64]); // RecordLength 0
    buf.extend_from_slice(&v2(31, 5, 0, 0, 0, "b.txt"));
    let r = parse_records(&buf);
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].name, "a.txt");
}

#[test]
fn truncated_record_stops() {
    let full = reply(0, &[v2(30, 5, 0, 0, 0, "a.txt"), v2(31, 5, 0, 0, 0, "longer-name.txt")]);
    for cut in 0..full.len() {
        let r = parse_records(&full[..cut]);
        assert!(r.len() <= 2);
        for x in &r {
            assert!(x.name == "a.txt" || x.name == "longer-name.txt");
        }
    }
    assert!(reply_next(&full[..7]).is_none());
}

#[test]
fn garbage_never_panics() {
    let mut seed = 0x1234_5678u32;
    for _ in 0..2000 {
        let len = (seed % 400) as usize;
        let buf: Vec<u8> = (0..len)
            .map(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                seed as u8
            })
            .collect();
        let _ = parse_records(&buf);
    }
    // A record length past the end, an unknown version, and a name outside the record.
    let mut big = v2(30, 5, 0, 0, 0, "x");
    big[0..4].copy_from_slice(&10_000u32.to_le_bytes());
    assert!(parse_records(&reply(0, &[big])).is_empty());
    let mut ver = v2(30, 5, 0, 0, 0, "x");
    ver[4] = 9;
    assert!(parse_records(&reply(0, &[ver, v2(31, 5, 0, 0, 0, "y")])).iter().map(|r| r.name.as_str()).eq(["y"]));
    let mut name = v2(30, 5, 0, 0, 0, "x");
    name[58..60].copy_from_slice(&500u16.to_le_bytes());
    assert!(parse_records(&reply(0, &[name])).is_empty());
}

#[test]
fn record_numbers_are_bounded() {
    assert_eq!(record_of(SEQ | 7), Some(7));
    assert_eq!(record_of(u64::from(MAX_RECORDS)), None);
    let mut v = VolumeIndex::new("C:");
    assert!(!v.insert(MAX_RECORDS, 5, "x", false));
    assert!(!v.insert(30, 5, "", false));
    assert_eq!(v.len(), 0);
}

// ---------------------------------------------------------------- index + journal

fn sample() -> VolumeIndex {
    let mut v = VolumeIndex::new("C:");
    v.add(&rec(5, 5, 0, true, "."));
    v.add(&rec(2, 5, 0, false, "$LogFile")); // metafile: skipped
    v.add(&rec(40, 5, 0, true, "Users"));
    v.add(&rec(41, 40, 0, true, "you"));
    v.add(&rec(42, 41, 0, false, "report.docx"));
    v
}

#[test]
fn builds_paths() {
    let v = sample();
    assert_eq!(v.len(), 3);
    assert_eq!(v.path(42).as_deref(), Some(r"C:\Users\you\report.docx"));
    assert_eq!(v.folder(42).as_deref(), Some(r"C:\Users\you"));
    assert_eq!(v.folder(40).as_deref(), Some(r"C:\"));
    assert!(v.is_dir(41) && !v.is_dir(42));
    assert!(v.name(2).is_none());
}

#[test]
fn paths_with_cycles_or_missing_parents_are_none() {
    let mut v = VolumeIndex::new("C:");
    v.insert(50, 51, "a", true);
    v.insert(51, 50, "b", true); // a <-> b
    assert_eq!(v.path(50), None);
    v.insert(60, 60, "self", true);
    assert_eq!(v.path(60), None);
    v.insert(70, 999, "orphan.txt", false);
    assert_eq!(v.path(70), None);
    // Deeper than MAX_DEPTH.
    let mut parent = 5;
    for i in 0..(MAX_DEPTH as u32 + 5) {
        v.insert(1000 + i, parent, "d", true);
        parent = 1000 + i;
    }
    assert_eq!(v.path(parent), None);
    assert!(v.path(1000 + 3).is_some());
}

#[test]
fn journal_create_rename_move_delete() {
    let mut v = sample();
    v.apply(&rec(43, 41, REASON_FILE_CREATE, false, "draft.txt"));
    assert_eq!(v.path(43).as_deref(), Some(r"C:\Users\you\draft.txt"));
    v.apply(&rec(43, 41, REASON_RENAME_OLD_NAME, false, "draft.txt"));
    v.apply(&rec(43, 41, REASON_RENAME_NEW_NAME, false, "final.txt"));
    assert_eq!(v.path(43).as_deref(), Some(r"C:\Users\you\final.txt"));
    v.apply(&rec(43, 40, REASON_RENAME_NEW_NAME, false, "final.txt")); // move
    assert_eq!(v.path(43).as_deref(), Some(r"C:\Users\final.txt"));
    assert_eq!(v.len(), 4);
    v.apply(&rec(43, 40, REASON_FILE_DELETE | 0x8000_0000, false, "final.txt"));
    assert_eq!(v.path(43), None);
    assert_eq!(v.len(), 3);
    // Moving a folder moves what is inside it.
    v.apply(&rec(41, 5, REASON_RENAME_NEW_NAME, true, "you"));
    assert_eq!(v.path(42).as_deref(), Some(r"C:\you\report.docx"));
    // Metafile records are ignored.
    v.apply(&rec(3, 5, REASON_FILE_CREATE, false, "$Volume"));
    assert!(v.name(3).is_none());
}

#[test]
fn reindex_triggers() {
    let j = JournalInfo { id: 9, first_usn: 1_000, next_usn: 5_000, lowest_valid_usn: 1_000 };
    assert_eq!(decide(9, 2_000, Some(&j)), Follow::From(2_000));
    assert_eq!(decide(8, 2_000, Some(&j)), Follow::Reindex, "journal id changed");
    assert_eq!(decide(9, 999, Some(&j)), Follow::Reindex, "usn older than the journal");
    let j2 = JournalInfo { lowest_valid_usn: 3_000, ..j };
    assert_eq!(decide(9, 2_000, Some(&j2)), Follow::Reindex, "usn below lowest valid");
    assert_eq!(decide(9, 2_000, None), Follow::Reindex, "journal not active");
}

// ---------------------------------------------------------------- ranking

#[test]
fn ranking_tiers() {
    let q = Query::new("Code");
    let exact = q.score("code");
    let prefix = q.score("Code.exe");
    let word = q.score("VS Code.lnk");
    let contains = q.score("barcode.png");
    assert!(exact > prefix && prefix > word && word > contains && contains > 0, "{exact} {prefix} {word} {contains}");
    let initials = Query::new("vsc").score("Visual Studio Code");
    let camel = Query::new("vsc").score("VisualStudioCode.exe");
    let fuzzy = Query::new("vsc").score("vlc_screens.png");
    assert!(initials > fuzzy && camel > fuzzy && fuzzy > 0, "{initials} {camel} {fuzzy}");
    assert!(initials < contains);
    assert_eq!(Query::new("xyz").score("chrome.exe"), -1);
    assert_eq!(Query::new("ab").score("axb"), -1, "two letters never fuzzy-match");
    assert!(Query::new("ÉTÉ").score("été.txt") > 7_000, "case-insensitive beyond ASCII");
    assert!(Query::new("code").score("code.exe") > Query::new("code").score("code-of-conduct-and-more.md"));
}

#[test]
fn top_k_keeps_the_best_and_searches_the_index() {
    let mut t = TopK::new(2);
    for (s, r) in [(5, 1), (9, 2), (1, 3), (7, 4)] {
        t.push(s, 0, r);
    }
    assert_eq!(t.into_sorted().iter().map(|x| x.2).collect::<Vec<_>>(), vec![2, 4]);

    let v = sample();
    let mut top = TopK::new(10);
    assert!(v.search(0, &Query::new("rep"), &mut top, &|| false));
    let hits = top.into_sorted();
    assert_eq!(hits.len(), 1);
    assert_eq!(v.path(hits[0].2).as_deref(), Some(r"C:\Users\you\report.docx"));
    let mut top = TopK::new(10);
    assert!(!v.search(0, &Query::new("rep"), &mut top, &|| true), "cancel stops the scan");
}

// ---------------------------------------------------------------- result ids

#[test]
fn result_ids_only_resolve_from_the_latest_reply() {
    let mut r = Results::default();
    let a = r.replace(vec![(r"C:\a.txt".into(), false), (r"C:\b".into(), true)]);
    assert_eq!(r.lookup(a), Some((r"C:\a.txt", false)));
    assert_eq!(r.lookup(a + 1), Some((r"C:\b", true)));
    assert_eq!(r.lookup(a + 2), None);
    assert_eq!(r.lookup(0), None);
    let b = r.replace(vec![(r"C:\c.txt".into(), false)]);
    assert_ne!(a, b);
    assert_eq!(r.lookup(a + 1), None, "an id from an older reply");
    assert_eq!(r.lookup(b), Some((r"C:\c.txt", false)));
    assert_eq!(r.lookup(u32::MAX), None);
}

// ---------------------------------------------------------------- keyboard layouts

#[test]
fn layout_maps_convert_between_layouts() {
    // VK_C 0x43, VK_H 0x48 ... on US and Arabic (101).
    let us: Vec<KeyChar> = vec![(0x43, false, 'c'), (0x48, false, 'h'), (0x52, false, 'r'), (0x4F, false, 'o'), (0x4D, false, 'm'), (0x45, false, 'e'), (0x43, true, 'C'), (0x20, false, ' '), (0x31, false, '1')];
    let ar: Vec<KeyChar> = vec![(0x43, false, 'ؤ'), (0x48, false, 'ا'), (0x52, false, 'ق'), (0x4F, false, 'خ'), (0x4D, false, 'ة'), (0x45, false, 'ث'), (0x43, true, '}'), (0x20, false, ' '), (0x31, false, '1')];
    let maps = layout_maps(&[("00000409".into(), us), ("00000401".into(), ar)]);
    assert_eq!(maps.len(), 2);
    let ar_to_us = maps.iter().find(|m| m.from == "00000401").unwrap();
    let typed: String = "ؤاقخةث".chars().map(|c| ar_to_us.map.get(&c.to_string()).cloned().unwrap_or(c.to_string())).collect();
    assert_eq!(typed, "chrome");
    assert_eq!(ar_to_us.map.get("}").map(String::as_str), Some("C"), "shift state");
    assert!(!ar_to_us.map.contains_key("1"), "same character: left out");
    assert!(!ar_to_us.map.contains_key(" "), "whitespace: left out");
    let us_to_ar = maps.iter().find(|m| m.from == "00000409").unwrap();
    assert_eq!(us_to_ar.map.get("c").map(String::as_str), Some("ؤ"));
    assert!(layout_maps(&[("00000409".into(), vec![(0x43, false, 'c')])]).is_empty(), "one layout: nothing to map");
}

// ---------------------------------------------------------------- review fixes

#[test]
fn search_sql_escapes_and_caps() {
    use super::wsearch::{search_sql, MAX_QUERY_CHARS};
    let like = |q: &str| {
        let sql = search_sql(q, 10);
        let start = sql.find("LIKE '%").unwrap() + 7;
        sql[start..sql.len() - 2].to_string()
    };
    assert_eq!(like("my_file"), "my[_]file");
    assert_eq!(like("50%"), "50[%]");
    assert_eq!(like("a[1]"), "a[[]1]");
    assert_eq!(like("it's \"x\""), "its x", "quotes removed");
    assert_eq!(like("ملف_été"), "ملف[_]été");
    assert!(search_sql("x", 10).starts_with("SELECT TOP 10 "));
    let long = like(&"a".repeat(10_000));
    assert_eq!(long.chars().count(), MAX_QUERY_CHARS);
    // 10k quotes and brackets: never more than 255 source characters, never a quote.
    let mixed = search_sql(&"'[_\"".repeat(2_500), 10);
    assert_eq!(mixed.matches('\'').count(), 4, "only the SQL's own quotes");
    assert!(like(&"[".repeat(10_000)).len() <= MAX_QUERY_CHARS * 3);
}

#[test]
fn rebuild_keeps_serving_the_old_index() {
    use std::sync::RwLock;
    assert_eq!(super::index_state(true, true), "ready", "a rebuild after the first build");
    assert_eq!(super::index_state(true, false), "indexing");
    assert_eq!(super::index_state(false, false), "notBuilt");
    let vols = RwLock::new(vec![sample()]);
    let mut fresh = VolumeIndex::new("C:");
    fresh.insert(40, 5, "Users", true);
    fresh.insert(90, 40, "new.txt", false);
    // Until the swap, the old index answers.
    assert_eq!(vols.read().unwrap()[0].path(42).as_deref(), Some(r"C:\Users\you\report.docx"));
    super::swap_volume(&vols, fresh);
    let v = vols.read().unwrap();
    assert_eq!(v.len(), 1);
    assert_eq!(v[0].path(90).as_deref(), Some(r"C:\Users\new.txt"));
    assert_eq!(v[0].path(42), None);
}

#[test]
fn journal_reply_with_many_records() {
    let recs: Vec<Vec<u8>> = (0..100u64).map(|i| v2(SEQ | (100 + i), 5, i as i64, REASON_FILE_CREATE, 0x20, &format!("f{i}.txt"))).collect();
    let buf = reply(4242, &recs);
    let parsed = parse_records(&buf);
    assert_eq!(parsed.len(), 100);
    let mut v = VolumeIndex::new("C:");
    for r in &parsed {
        v.apply(r);
    }
    assert_eq!(v.len(), 100);
    assert_eq!(v.path(199).as_deref(), Some(r"C:\f99.txt"));
}

#[test]
fn one_windows_search_query_in_flight() {
    use super::wsearch::OneInFlight;
    use std::sync::{Arc, Mutex, OnceLock};
    use std::time::Duration;
    static RAN: OnceLock<Mutex<Vec<u32>>> = OnceLock::new();
    static ACTIVE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    static MAX: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    use std::sync::atomic::Ordering::SeqCst;
    fn run(j: u32) {
        let now = ACTIVE.fetch_add(1, SeqCst) + 1;
        MAX.fetch_max(now, SeqCst);
        std::thread::sleep(Duration::from_millis(if j == 1 { 300 } else { 20 }));
        RAN.get_or_init(Default::default).lock().unwrap().push(j);
        ACTIVE.fetch_sub(1, SeqCst);
    }
    let q = Arc::new(OneInFlight::<u32>::default());
    OneInFlight::submit(&q, 1, run);
    std::thread::sleep(Duration::from_millis(50)); // 1 is running
    for j in 2..=5 {
        OneInFlight::submit(&q, j, run);
    }
    std::thread::sleep(Duration::from_millis(600));
    assert_eq!(*RAN.get().unwrap().lock().unwrap(), vec![1, 5], "intermediate queries dropped");
    assert_eq!(MAX.load(SeqCst), 1, "never two at once");
    OneInFlight::submit(&q, 6, run); // idle again: starts at once
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(*RAN.get().unwrap().lock().unwrap(), vec![1, 5, 6]);
}

#[test]
fn stale_replies_are_not_stored() {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Mutex;
    let results = Mutex::new(Results::default());
    let gen = AtomicU64::new(7);
    let first = super::store_if_current(&results, &gen, 7, vec![(r"C:\a.txt".into(), false)]).unwrap();
    gen.store(8, Ordering::SeqCst); // a newer query started
    assert_eq!(super::store_if_current(&results, &gen, 7, vec![(r"C:\b.txt".into(), false)]), None);
    assert_eq!(results.lock().unwrap().lookup(first), Some((r"C:\a.txt", false)), "stale reply left the stored results alone");
}
