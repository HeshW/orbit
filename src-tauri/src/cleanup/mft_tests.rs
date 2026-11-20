//! Byte fixtures only; never a live disk.

use super::*;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

const SZ: usize = 1024;

fn attr_res(kind: u32, name: &[u16], value: &[u8]) -> Vec<u8> {
    let name_off = 0x18;
    let val_off = (name_off + name.len() * 2 + 7) & !7;
    let len = (val_off + value.len() + 7) & !7;
    let mut a = vec![0u8; len];
    a[0..4].copy_from_slice(&kind.to_le_bytes());
    a[4..8].copy_from_slice(&(len as u32).to_le_bytes());
    a[9] = name.len() as u8;
    a[0xA..0xC].copy_from_slice(&(name_off as u16).to_le_bytes());
    a[0x10..0x14].copy_from_slice(&(value.len() as u32).to_le_bytes());
    a[0x14..0x16].copy_from_slice(&(val_off as u16).to_le_bytes());
    for (i, u) in name.iter().enumerate() {
        a[name_off + i * 2..name_off + i * 2 + 2].copy_from_slice(&u.to_le_bytes());
    }
    a[val_off..val_off + value.len()].copy_from_slice(value);
    a
}

fn std_info(attrs: u32) -> Vec<u8> {
    let mut v = vec![0u8; 0x48];
    v[0x20..0x24].copy_from_slice(&attrs.to_le_bytes());
    attr_res(0x10, &[], &v)
}

fn file_name(parent: u64, seq: u16, ns: u8, name: &str) -> Vec<u8> {
    let units: Vec<u16> = name.encode_utf16().collect();
    let mut v = vec![0u8; 0x42 + units.len() * 2];
    v[0..8].copy_from_slice(&(parent | (u64::from(seq) << 48)).to_le_bytes());
    v[0x40] = units.len() as u8;
    v[0x41] = ns;
    for (i, u) in units.iter().enumerate() {
        v[0x42 + i * 2..0x44 + i * 2].copy_from_slice(&u.to_le_bytes());
    }
    attr_res(0x30, &[], &v)
}

fn data_nonres(lowest: u64, alloc: u64, total: Option<u64>, runs: &[u8]) -> Vec<u8> {
    let head = if total.is_some() { 0x48 } else { 0x40 };
    let len = (head + runs.len() + 7) & !7;
    let mut a = vec![0u8; len];
    a[0..4].copy_from_slice(&0x80u32.to_le_bytes());
    a[4..8].copy_from_slice(&(len as u32).to_le_bytes());
    a[8] = 1;
    a[0xA..0xC].copy_from_slice(&(head as u16).to_le_bytes());
    a[0x10..0x18].copy_from_slice(&lowest.to_le_bytes());
    a[0x20..0x22].copy_from_slice(&(head as u16).to_le_bytes());
    a[0x28..0x30].copy_from_slice(&alloc.to_le_bytes());
    a[0x30..0x38].copy_from_slice(&alloc.to_le_bytes());
    if let Some(t) = total {
        a[0xC..0xE].copy_from_slice(&0x8000u16.to_le_bytes()); // sparse
        a[0x22..0x24].copy_from_slice(&4u16.to_le_bytes());
        a[0x40..0x48].copy_from_slice(&t.to_le_bytes());
    }
    a[head..head + runs.len()].copy_from_slice(runs);
    a
}

const ONE_RUN: &[u8] = &[0x11, 0x01, 0x10, 0x00];

/// A record with fix-ups applied, as it sits on disk.
fn record(seq: u16, flags: u16, base: u64, attrs: &[Vec<u8>]) -> Vec<u8> {
    let mut r = vec![0u8; SZ];
    r[0..4].copy_from_slice(b"FILE");
    r[4..6].copy_from_slice(&0x30u16.to_le_bytes());
    r[6..8].copy_from_slice(&3u16.to_le_bytes());
    r[0x10..0x12].copy_from_slice(&seq.to_le_bytes());
    r[0x14..0x16].copy_from_slice(&0x38u16.to_le_bytes());
    r[0x16..0x18].copy_from_slice(&flags.to_le_bytes());
    r[0x20..0x28].copy_from_slice(&base.to_le_bytes());
    let mut at = 0x38;
    for a in attrs {
        r[at..at + a.len()].copy_from_slice(a);
        at += a.len();
    }
    r[at..at + 4].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
    r[0x18..0x1C].copy_from_slice(&((at + 8) as u32).to_le_bytes());
    r[0x1C..0x20].copy_from_slice(&(SZ as u32).to_le_bytes());
    // Update sequence: USN 0x0707, real sector tails saved in the array.
    let usn = 0x0707u16;
    r[0x30..0x32].copy_from_slice(&usn.to_le_bytes());
    for i in 1..3 {
        let pos = i * 512 - 2;
        let orig = [r[pos], r[pos + 1]];
        r[0x30 + i * 2..0x32 + i * 2].copy_from_slice(&orig);
        r[pos..pos + 2].copy_from_slice(&usn.to_le_bytes());
    }
    r
}

const FILE: u16 = 1;
const FOLDER: u16 = 3;

fn dir_rec(parent: u64, name: &str) -> Vec<u8> {
    record(1, FOLDER, 0, &[std_info(0x10), file_name(parent, 1, 1, name)])
}

fn file_rec(parent: u64, name: &str, alloc: u64) -> Vec<u8> {
    record(1, FILE, 0, &[std_info(0x20), file_name(parent, 1, 1, name), data_nonres(0, alloc, None, ONE_RUN)])
}

fn table(recs: Vec<(usize, Vec<u8>)>) -> Table {
    let n = recs.iter().map(|r| r.0).max().unwrap_or(0) + 1;
    let mut slots = vec![vec![0u8; SZ]; n];
    for (no, r) in recs {
        slots[no] = r;
    }
    // Through Feed, split at an odd boundary, like chunked reads.
    let all: Vec<u8> = slots.concat();
    let mut feed = Feed::new(SZ);
    let cut = all.len() / 2 + 300;
    feed.push(&all[..cut]);
    feed.push(&all[cut..]);
    let mut t = feed.table;
    t.finish();
    t
}

fn with_ctl<R>(f: impl FnOnce(&mut Ctl<'_>) -> R) -> R {
    let cancel = AtomicBool::new(false);
    let mut sink = |_: &super::super::Progress| {};
    let mut ctl = Ctl::new("folders", &cancel, &mut sink);
    f(&mut ctl)
}

#[test]
fn fixups_applied_and_torn_records_refused() {
    let mut r = file_rec(5, "a.bin", 4096);
    assert_eq!(u16_at(&r, 510), Some(0x0707));
    assert!(fixup(&mut r));
    assert_eq!(u16_at(&r, 510), Some(0));
    let mut torn = file_rec(5, "a.bin", 4096);
    torn[1022] = 0x99;
    assert!(!fixup(&mut torn));
    let mut short = vec![0u8; 8];
    assert!(!fixup(&mut short));
}

#[test]
fn fragmented_run_list() {
    // 0x10 clusters at 0x1000, then 8 clusters 0x200 back (negative delta), then sparse 4.
    let runs = [0x21, 0x10, 0x00, 0x10, 0x21, 0x08, 0x00, 0xFE, 0x01, 0x04, 0x00];
    assert_eq!(data_runs(&runs), Some(vec![(Some(0x1000), 0x10), (Some(0xE00), 8), (None, 4)]));
    assert_eq!(data_runs(&[0x21, 0x10]), None, "truncated run is refused");
    assert_eq!(data_runs(&[0x91, 0x01, 0x00]), None, "oversized field is refused");
}

#[test]
fn record_details() {
    let root = 5;
    let mut recs = vec![
        (root, record(1, FOLDER, 0, &[std_info(0x16), file_name(5, 1, 3, ".")])),
        (2, record(2, FILE, 0, &[file_name(5, 1, 3, "$LogFile"), data_nonres(0, 1 << 26, None, ONE_RUN)])),
        (30, dir_rec(5, "A")),
        (31, file_rec(30, "plain.bin", 4096)),
        // Sparse: allocated 64 KiB logical, 8 KiB actually on disk.
        (32, record(1, FILE, 0, &[std_info(0x200), file_name(30, 1, 1, "sparse.bin"), data_nonres(0, 65536, Some(8192), ONE_RUN)])),
        // Base 33 holds only std info; its name and data live in extension 34.
        (33, record(1, FILE, 0, &[std_info(0x20)])),
        (34, record(1, FILE, 33, &[file_name(30, 1, 1, "spilled.bin"), data_nonres(0, 12288, None, ONE_RUN)])),
        // A later extension segment (lowest VCN > 0) must not overwrite the size.
        (35, record(1, FILE, 33, &[data_nonres(3, 999, None, ONE_RUN)])),
        // Hard link: two Win32 names, counted once under the first (A).
        (36, record(1, FILE, 0, &[std_info(0x20), file_name(30, 1, 1, "link1"), file_name(5, 1, 1, "link2"), data_nonres(0, 1000, None, ONE_RUN)])),
        // Junction under the root: not followed.
        (37, record(1, FOLDER, 0, &[std_info(0x410), file_name(5, 1, 1, "Junction"), attr_res(0xC0, &[], &[0; 8])])),
        (38, file_rec(37, "behind-junction.bin", 1 << 30)),
        // DOS-only name is ignored; the Win32 name is used.
        (39, record(1, FILE, 0, &[std_info(0x20), file_name(5, 1, 2, "LONGNA~1.TXT"), file_name(5, 1, 1, "long name.txt"), attr_res(0x80, &[], b"tiny")])),
        (40, file_rec(5, "free.bin", 777)),
        // Stale parent reference (sequence 9, folder A has 1).
        (41, record(1, FILE, 0, &[file_name(30, 9, 1, "stale.bin"), data_nonres(0, 5555, None, ONE_RUN)])),
        (42, vec![0xAB; SZ]),
        (43, file_rec(5, "torn.bin", 1 << 20)),
        // Offline (cloud placeholder) attributes: size counts 0, like the walk.
        (44, record(1, FILE, 0, &[std_info(0x0040_0000), file_name(5, 1, 1, "cloud.bin"), data_nonres(0, 4096, None, ONE_RUN)])),
    ];
    recs.iter_mut().find(|r| r.0 == 40).unwrap().1[0x16] = 0; // not in use
    recs.iter_mut().find(|r| r.0 == 43).unwrap().1[1022] = 0x11; // torn
    // Garbage that claims to be a record with an attribute running off the end.
    let mut bad = record(1, FILE, 0, &[std_info(0x20)]);
    bad[0x38 + 4..0x38 + 8].copy_from_slice(&5000u32.to_le_bytes());
    recs.push((45, bad));
    let t = table(recs);
    let (top, kept) = with_ctl(|ctl| t.sizes(Path::new(r"X:\"), &[], ctl)).unwrap();
    assert_eq!(top.links, 1);
    assert_eq!(top.files_here, 2, "long name.txt and cloud.bin");
    assert_eq!(top.files_here_bytes, 0, "resident data and offline files take no extra space");
    assert_eq!(top.folders.len(), 1);
    let a = &top.folders[0];
    assert_eq!((a.name.as_str(), a.bytes, a.files), ("A", 4096 + 8192 + 12288 + 1000, 4));
    assert_eq!(top.bytes, a.bytes);
    assert_eq!(kept.len(), 1);
    let (sub, _) = with_ctl(|ctl| t.sizes(Path::new(r"X:\a"), &["a".to_string()], ctl)).unwrap();
    assert_eq!(sub.files_here, 4);
    assert!(with_ctl(|ctl| t.sizes(Path::new(r"X:\Junction"), &["Junction".to_string()], ctl)).is_err());
}

#[test]
fn cancel_stops_with_blank_result() {
    let t = table(vec![(5, dir_rec(5, ".")), (30, file_rec(5, "a", 1))]);
    let cancel = AtomicBool::new(true);
    let mut sink = |_: &super::super::Progress| {};
    let mut ctl = Ctl::new("folders", &cancel, &mut sink);
    let (top, kept) = t.sizes(Path::new(r"X:\"), &[], &mut ctl).unwrap();
    assert!(top.cancelled && kept.is_empty());
}

/// The same tree measured by the walk (on real temp files) and from records.
#[test]
fn same_result_as_the_walk() {
    let target = std::env::var("CARGO_TARGET_DIR")
        .map(|t| PathBuf::from(t.replace('/', "\\")))
        .unwrap_or_else(|_| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target"));
    let b = target.join("cleanup-tests").join(format!("mft-{}", std::process::id()));
    let _ = fs::remove_dir_all(&b);
    // (relative path, size); folders end in '\'.
    let mut tree: Vec<(String, u64)> = vec![
        (r"Big\".into(), 0),
        (r"Big\a\".into(), 0),
        (r"Big\a\deep\".into(), 0),
        (r"Big\a\deep\deeper\".into(), 0),
        (r"Big\a\deep\deeper\x.bin".into(), 900),
        (r"Big\a\y.bin".into(), 500),
        (r"Big\b.bin".into(), 300),
        (r"Empty\".into(), 0),
        ("loose.bin".into(), 7),
    ];
    for i in 0..45 {
        tree.push((format!(r"f{i:02}\"), 0));
        tree.push((format!(r"f{i:02}\z.bin"), i + 1));
    }
    let mut recs = vec![(5usize, dir_rec(5, "."))];
    let mut ids: std::collections::HashMap<String, u64> = Default::default();
    for (i, (rel, size)) in tree.iter().enumerate() {
        let no = 100 + i;
        let trimmed = rel.trim_end_matches('\\');
        let (parent, name) = match trimmed.rsplit_once('\\') {
            Some((p, n)) => (ids[p], n),
            None => (5, trimmed),
        };
        let p = b.join(trimmed);
        if rel.ends_with('\\') {
            fs::create_dir_all(&p).unwrap();
            ids.insert(trimmed.to_string(), no as u64);
            recs.push((no, dir_rec(parent, name)));
        } else {
            fs::write(&p, vec![1u8; *size as usize]).unwrap();
            recs.push((no, file_rec(parent, name, *size)));
        }
    }
    let walk = with_ctl(|ctl| super::super::folder_sizes(&b, ctl));
    let t = table(recs);
    let mft = with_ctl(|ctl| t.sizes(&b, &[], ctl)).unwrap();
    let norm = |r: (FolderSizes, Vec<FolderSizes>)| {
        let mut all = vec![r.0];
        let mut rest = r.1;
        rest.sort_by(|x, y| x.path.cmp(&y.path));
        all.extend(rest);
        all.into_iter()
            .map(|mut f| {
                f.scanned_at = 0;
                serde_json::to_value(f).unwrap()
            })
            .collect::<Vec<_>>()
    };
    let (w, m) = (norm(walk), norm(mft));
    assert_eq!(w[0]["otherCount"], 7);
    assert_eq!(w, m);
    let _ = fs::remove_dir_all(&b);
}
