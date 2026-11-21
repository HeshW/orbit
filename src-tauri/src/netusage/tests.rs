use super::*;

const T0: u64 = 1_800_000_000 - 1_800_000_000 % BUCKET_SECS; // a bucket start
const DAY: i64 = 20_833;

fn app<'a>(key: &str, name: &'a str, rx: f64, tx: f64) -> TickApp<'a> {
    TickApp { key: key.to_string(), name, exe: None, rx, tx }
}

#[test]
fn ticks_land_in_five_minute_buckets_and_days() {
    let mut s = Store::default();
    s.add(T0 + 10, DAY, 100.0, 10.0, vec![app("chrome.exe", "chrome.exe", 60.0, 5.0)]);
    s.add(T0 + 299, DAY, 100.0, 10.0, vec![app("chrome.exe", "chrome.exe", 40.0, 5.0)]);
    s.add(T0 + 300, DAY, 50.0, 0.0, vec![]);
    assert_eq!(s.buckets.len(), 2);
    assert_eq!(s.buckets[0].start, T0);
    assert_eq!(s.buckets[0].first, T0 + 10);
    assert_eq!(s.buckets[0].rx, 200.0);
    assert_eq!(s.buckets[0].apps["chrome.exe"], [100.0, 10.0]);
    assert_eq!(s.buckets[1].start, T0 + 300);
    assert_eq!(s.days, vec![DayTotal { day: DAY, first: T0 + 10, rx: 250.0, tx: 20.0 }]);
    assert_eq!(s.names["chrome.exe"].name, "chrome.exe");
}

#[test]
fn a_clock_set_back_keeps_buckets_in_order() {
    let mut s = Store::default();
    s.add(T0 + 600, DAY, 1.0, 0.0, vec![]);
    s.add(T0 + 5, DAY, 2.0, 0.0, vec![]);
    s.add(T0 + 610, DAY, 4.0, 0.0, vec![]);
    let starts: Vec<u64> = s.buckets.iter().map(|b| b.start).collect();
    assert_eq!(starts, vec![T0, T0 + 600]);
    assert_eq!(s.buckets[1].rx, 5.0);
    assert_eq!(s.buckets[0].rx, 2.0);
}

#[test]
fn recent_counts_buckets_that_overlap_the_window() {
    let mut s = Store::default();
    // 2 hours of buckets, 1 byte down each, chrome in every other one.
    for i in 0..24u64 {
        let apps = if i % 2 == 0 { vec![app("chrome.exe", "Chrome", 10.0, 1.0)] } else { vec![] };
        s.add(T0 + i * BUCKET_SECS, DAY, 1.0, 2.0, apps);
    }
    let now = T0 + 23 * BUCKET_SECS + 100;
    let (rx, tx, since, apps) = s.recent(now, 3600);
    // The window starts 100 s into the 12th bucket from the end: 13 buckets overlap.
    assert_eq!(rx, 13.0);
    assert_eq!(tx, 26.0);
    assert_eq!(since, Some(T0 + 11 * BUCKET_SECS));
    assert_eq!(apps.len(), 1);
    assert_eq!(apps[0].name, "Chrome");
    assert_eq!((apps[0].rx_bytes, apps[0].tx_bytes), (60, 6));
    let (rx, _, _, _) = s.recent(now, 86_400);
    assert_eq!(rx, 24.0);
}

#[test]
fn recent_with_nothing_is_empty() {
    let s = Store::default();
    let (rx, tx, since, apps) = s.recent(T0, 3600);
    assert_eq!((rx, tx, since), (0.0, 0.0, None));
    assert!(apps.is_empty());
}

#[test]
fn days_total_sums_the_range() {
    let mut s = Store::default();
    for d in 0..10 {
        s.add(T0 + d as u64 * 86_400, DAY + d, 100.0, 1.0, vec![]);
    }
    let (rx, tx, since) = s.days_total(DAY + 3, DAY + 9);
    assert_eq!((rx, tx), (700.0, 7.0));
    assert_eq!(since, Some(T0 + 3 * 86_400));
    assert_eq!(s.days_total(DAY + 20, DAY + 21), (0.0, 0.0, None));
}

#[test]
fn prune_keeps_a_day_of_buckets_and_thirty_days() {
    let mut s = Store::default();
    for d in 0..40 {
        s.add(T0 + d as u64 * 86_400, DAY + d, 1.0, 0.0, vec![]);
    }
    s.add(T0 + 39 * 86_400 + 600, DAY + 39, 1.0, 0.0, vec![app("old.exe", "old.exe", 1.0, 0.0)]);
    let now = T0 + 40 * 86_400 + 300;
    s.prune(now, DAY + 40);
    // Day 39's buckets started 24 h + 300 s and 24 h - 300 s ago: only the second overlaps.
    assert_eq!(s.buckets.len(), 1);
    assert_eq!(s.buckets[0].start, T0 + 39 * 86_400 + 600);
    assert_eq!(s.days.first().map(|d| d.day), Some(DAY + 11));
    assert_eq!(s.days.len(), 29);
    assert!(s.names.contains_key("old.exe"));
    s.prune(now + 86_400, DAY + 41);
    assert!(s.buckets.is_empty());
    assert!(s.names.is_empty(), "names no bucket uses are dropped");
}

#[test]
fn merge_adds_the_file_to_what_was_recorded_first() {
    let mut file = Store::default();
    file.add(T0, DAY, 10.0, 1.0, vec![app("a.exe", "a.exe", 5.0, 0.0)]);
    file.add(T0 - BUCKET_SECS, DAY, 3.0, 0.0, vec![]);
    let mut mem = Store::default();
    mem.add(T0 + 20, DAY, 1.0, 1.0, vec![app("a.exe", "a.exe", 1.0, 1.0), app("b.exe", "b.exe", 2.0, 0.0)]);
    mem.merge(file);
    assert_eq!(mem.buckets.len(), 2);
    assert_eq!(mem.buckets[0].start, T0 - BUCKET_SECS);
    assert_eq!(mem.buckets[1].rx, 11.0);
    assert_eq!(mem.buckets[1].first, T0);
    assert_eq!(mem.buckets[1].apps["a.exe"], [6.0, 1.0]);
    assert_eq!(mem.buckets[1].apps["b.exe"], [2.0, 0.0]);
    assert_eq!(mem.days.len(), 1);
    assert_eq!(mem.days[0].rx, 14.0);
}

#[test]
fn forget_drops_one_app_and_keeps_machine_totals() {
    let mut s = Store::default();
    s.add(T0, DAY, 10.0, 0.0, vec![app("a.exe", "a.exe", 5.0, 0.0), app("b.exe", "b.exe", 1.0, 0.0)]);
    s.forget("a.exe");
    assert!(!s.names.contains_key("a.exe"));
    assert!(!s.buckets[0].apps.contains_key("a.exe"));
    assert!(s.buckets[0].apps.contains_key("b.exe"));
    assert_eq!(s.buckets[0].rx, 10.0);
}

#[test]
fn file_round_trips_and_a_broken_file_is_skipped() {
    let dir = std::env::temp_dir().join(format!("orbit-netusage-test-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    let mut s = Store::default();
    s.add(T0, DAY, 10.0, 2.0, vec![app("a.exe", "A", 5.0, 1.0)]);
    write_file(&dir, &mut s).unwrap();
    let back = read_file(&dir).unwrap();
    assert_eq!(back, s);
    assert_eq!(back.version, FILE_VERSION);
    fs::write(dir.join(FILE_NAME), b"{ not json").unwrap();
    assert!(read_file(&dir).is_none());
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn tick_apps_groups_by_app_and_skips_rows_without_traffic() {
    use crate::model::*;
    let row = |pid: u32, name: &str, exe: Option<&str>, rx: Option<f64>, tx: Option<f64>| ProcessRow {
        pid,
        parent_pid: None,
        name: name.into(),
        exe: exe.map(Into::into),
        cmd: String::new(),
        user: None,
        cpu: 0.0,
        memory: 0,
        disk_read_bps: 0.0,
        disk_write_bps: 0.0,
        gpu: None,
        status: "running".into(),
        start_time: 1,
        kind: ProcessKind::Background,
        window_title: None,
        not_responding: false,
        suspended: false,
        session_id: Some(1),
        net_rx_bps: rx,
        net_tx_bps: tx,
    };
    let snap = Snapshot {
        ts: 0,
        interval_ms: 1000,
        cpu: CpuStats { usage: 0.0, per_core: vec![0.0], freq_mhz: 0 },
        memory: MemoryStats {
            total: 1,
            used: 1,
            available: 0,
            swap_total: 0,
            swap_used: 0,
            committed: None,
            commit_limit: None,
            cached: None,
            free: None,
        },
        disk: DiskStats { active_pct: None, read_bps: None, write_bps: None },
        network: NetworkStats { rx_bps: 0.0, tx_bps: 0.0, interfaces: vec![], per_app: PerAppStatus::default() },
        gpu: GpuStats { usage: None, dedicated_used: None, adapters: Vec::new() },
        system: SystemStats { uptime_secs: 1, process_count: 0, thread_count: None, handle_count: None },
        disks: vec![],
        gpu_engines: vec![],
        battery: None,
        sensors: Default::default(),
        processes: Vec::new(),
    };
    let mut snap = snap;
    snap.processes = vec![
        row(10, "chrome.exe", Some("C:\\Apps\\Chrome.exe"), Some(100.0), Some(10.0)),
        row(11, "chrome.exe", Some("C:\\APPS\\chrome.exe"), Some(50.0), Some(0.0)),
        row(12, "idle.exe", None, Some(0.0), Some(0.0)),
        row(13, "nodata.exe", None, None, None),
        row(0, "System Idle Process", None, Some(5.0), None),
        row(14, "svc.exe", None, Some(f64::NAN), Some(4.0)),
    ];
    let apps = tick_apps(&snap, 2.0);
    assert_eq!(apps.len(), 2);
    assert_eq!(apps[0].key, "c:\\apps\\chrome.exe");
    assert_eq!((apps[0].rx, apps[0].tx), (300.0, 20.0));
    assert_eq!(apps[1].key, "svc.exe");
    assert_eq!((apps[1].rx, apps[1].tx), (0.0, 8.0));
}

fn row(rx: u64, tx: u64, first_seen: u64) -> history::HistoryRow {
    history::HistoryRow {
        key: "a.exe".into(),
        name: "a.exe".into(),
        exe: None,
        cpu_seconds: 0.0,
        max_memory: 0,
        avg_memory: 0,
        disk_read_bytes: 0,
        disk_write_bytes: 0,
        net_rx_bytes: rx,
        net_tx_bytes: tx,
        active_seconds: 0.0,
        first_seen,
        last_seen: first_seen,
    }
}

#[test]
fn long_period_totals_are_never_less_than_the_apps() {
    // Day totals started later than App history: the apps' sum and start win.
    let rows = [row(6_000, 100, T0 - 5 * 86_400), row(2_000, 50, 0)];
    assert_eq!(with_app_totals(300.0, 40.0, Some(T0), &rows), (8_000.0, 150.0, Some(T0 - 5 * 86_400)));
    // Day totals bigger (traffic no app was counted for): they stay.
    assert_eq!(with_app_totals(9_000.0, 400.0, Some(T0 - 86_400), &rows), (9_000.0, 400.0, Some(T0 - 5 * 86_400)));
    // No apps.
    assert_eq!(with_app_totals(10.0, 5.0, Some(T0), &[]), (10.0, 5.0, Some(T0)));
    assert_eq!(with_app_totals(0.0, 0.0, None, &[row(1, 1, T0)]), (1.0, 1.0, Some(T0)));
}

fn raw(id: &str, rx: u64, tx: u64) -> winrecord::Raw {
    winrecord::Raw { id: id.into(), rx, tx }
}

#[test]
fn windows_record_ids_become_app_rows() {
    let devices = vec![(r"\Device\HarddiskVolume3".to_string(), "C:".to_string())];
    let mut known = HashMap::new();
    known.insert(
        r"c:\program files\edge\msedge.exe".to_string(),
        ("msedge.exe".to_string(), Some(r"C:\Program Files\Edge\msedge.exe".to_string())),
    );
    let mut apps = winrecord::to_apps(
        &[
            // Windows sends lowercase device paths; App history's name and casing win.
            raw(r"\device\harddiskvolume3\program files\edge\msedge.exe", 900, 10),
            raw(r"\device\harddiskvolume3\games\game.exe", 500, 5),
            raw(r"\device\harddiskvolume3\GAMES\Game.exe", 100, 1),
            raw(r"\device\harddiskvolume9\gone\old.exe", 7, 0),
            raw("Claude_pzs8sxrjxfjjc", 300, 30),
            raw("", 40, 4),
            raw("System", 2, 2),
            raw("Microsoft.WindowsStore_8wekyb3d8bbwe", 0, 0),
        ],
        &devices,
        &known,
    );
    apps.sort_by(|a, b| a.key.cmp(&b.key));
    let got: Vec<_> = apps.iter().map(|a| (a.key.as_str(), a.name.as_str(), a.exe.as_deref(), a.rx_bytes, a.tx_bytes)).collect();
    assert_eq!(
        got,
        vec![
            (r"\device\harddiskvolume9\gone\old.exe", "old.exe", None, 7, 0),
            (r"c:\games\game.exe", "game.exe", Some(r"C:\games\game.exe"), 600, 6),
            (r"c:\program files\edge\msedge.exe", "msedge.exe", Some(r"C:\Program Files\Edge\msedge.exe"), 900, 10),
            ("claude_pzs8sxrjxfjjc", "Claude", None, 300, 30),
            ("system", "System", None, 2, 2),
            (winrecord::UNATTRIBUTED, "Not tied to an app", None, 40, 4),
        ]
    );
}

#[test]
fn windows_totals_are_the_apps_unless_orbit_counted_more() {
    let apps = [
        UsageApp { key: "a".into(), name: "a".into(), exe: None, rx_bytes: 1_000, tx_bytes: 50 },
        UsageApp { key: "b".into(), name: "b".into(), exe: None, rx_bytes: 500, tx_bytes: 50 },
    ];
    assert_eq!(with_windows_totals(10.0, 400.0, &apps), (1_500.0, 400.0));
    assert_eq!(with_windows_totals(0.0, 0.0, &[]), (0.0, 0.0));
}

#[test]
fn unknown_period_is_an_error() {
    assert!(usage("year", T0).is_err());
}
