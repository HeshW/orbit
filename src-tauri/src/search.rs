//! File search for the Ctrl+K palette (v1.4). As administrator: an in-memory
//! name index of every fixed NTFS drive, built on the first file query from
//! FSCTL_ENUM_USN_DATA and kept current from the change journal while Orbit
//! runs. Without administrator: the Windows Search index. Results open through
//! the user's own (unelevated) shell. The index is never written to disk.
//! Also: keyboard-layout maps so text typed on the wrong layout still matches.

pub mod index;
pub mod launch;
pub mod usn;
pub mod wsearch;

#[cfg(test)]
mod tests;

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock, RwLock};
use std::time::{Duration, Instant};

use serde::Serialize;

use index::{decide, Follow, Query, Results, TopK, VolumeIndex};

pub const MAX_RESULTS: usize = 50;
const WSEARCH_BUDGET: Duration = Duration::from_secs(2);
const FOLLOW_EVERY: Duration = Duration::from_secs(2);
/// A drive with no journal is read again at most this often (on a query).
const UNFOLLOWED_REFRESH: Duration = Duration::from_secs(600);

#[derive(Serialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct SearchStatus {
    /// "index" (administrator) or "windowsSearch".
    pub route: &'static str,
    /// "notBuilt" | "indexing" | "ready" | "unavailable"
    pub state: &'static str,
    pub drive: Option<String>,
    pub percent: Option<u8>,
    pub files: usize,
    pub memory_bytes: usize,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct FileResult {
    pub id: u32,
    pub name: String,
    pub folder: String,
    pub is_dir: bool,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct FileSearchReply {
    pub query: String,
    /// A newer query started; ignore this reply.
    pub stale: bool,
    pub status: SearchStatus,
    pub results: Vec<FileResult>,
}

#[derive(Default)]
struct Progress {
    building: bool,
    built: bool,
    drive: Option<String>,
    percent: Option<u8>,
    built_at: Option<Instant>,
}

#[derive(Default)]
struct Search {
    volumes: RwLock<Vec<VolumeIndex>>,
    progress: Mutex<Progress>,
    gen: AtomicU64,
    results: Mutex<Results>,
    follower: AtomicBool,
}

fn state() -> &'static Search {
    static S: OnceLock<Search> = OnceLock::new();
    S.get_or_init(Search::default)
}

fn status() -> SearchStatus {
    let s = state();
    if !launch::is_elevated() {
        let ok = wsearch::service_running();
        return SearchStatus { route: "windowsSearch", state: if ok { "ready" } else { "unavailable" }, ..Default::default() };
    }
    let p = s.progress.lock().unwrap_or_else(|e| e.into_inner());
    let vols = s.volumes.read().unwrap_or_else(|e| e.into_inner());
    let state = index_state(p.building, p.built);
    SearchStatus {
        route: "index",
        state,
        drive: if state == "indexing" { p.drive.clone() } else { None },
        percent: if state == "indexing" { p.percent } else { None },
        files: vols.iter().map(VolumeIndex::len).sum(),
        memory_bytes: vols.iter().map(VolumeIndex::memory_bytes).sum(),
    }
}

/// A rebuild keeps serving the index it replaces: "indexing" only before the
/// first build is done.
pub(crate) fn index_state(building: bool, built: bool) -> &'static str {
    if built {
        "ready"
    } else if building {
        "indexing"
    } else {
        "notBuilt"
    }
}

/// Swaps in a freshly built volume (or adds it); searches keep using the old
/// one until this moment.
pub(crate) fn swap_volume(vols: &RwLock<Vec<VolumeIndex>>, fresh: VolumeIndex) {
    let mut vols = vols.write().unwrap_or_else(|e| e.into_inner());
    match vols.iter_mut().find(|v| v.drive == fresh.drive) {
        Some(v) => *v = fresh,
        None => vols.push(fresh),
    }
}

/// Stores a reply's paths only if no newer query started; the check and the
/// store happen under the same lock. Returns the first id, or None when stale.
pub(crate) fn store_if_current(results: &Mutex<Results>, gen_now: &AtomicU64, gen: u64, paths: Vec<(String, bool)>) -> Option<u32> {
    let mut r = results.lock().unwrap_or_else(|e| e.into_inner());
    if gen_now.load(Ordering::SeqCst) != gen {
        return None;
    }
    Some(r.replace(paths))
}

/// Reads one volume from scratch, reporting progress.
fn build_volume(drive: &str, progress: &dyn Fn(u8)) -> Result<VolumeIndex, String> {
    let f = usn::open_volume(drive)?;
    let journal = usn::query_journal(&f)?;
    let total = usn::record_estimate(&f).max(1);
    let mut vol = VolumeIndex::new(drive);
    let mut seen = 0u64;
    usn::enumerate(&f, journal.map(|j| j.next_usn).unwrap_or(i64::MAX), |batch| {
        for r in batch {
            vol.add(r);
        }
        seen += batch.len() as u64;
        progress(((seen * 100) / total).min(99) as u8);
        true
    })?;
    if let Some(j) = journal {
        vol.journal_id = j.id;
        vol.next_usn = j.next_usn;
        vol.followed = true;
    }
    Ok(vol)
}

fn start_build() {
    let s = state();
    {
        let mut p = s.progress.lock().unwrap_or_else(|e| e.into_inner());
        if p.building {
            return;
        }
        p.building = true;
    }
    std::thread::spawn(|| {
        let s = state();
        let mut built = Vec::new();
        for drive in usn::fixed_ntfs_volumes() {
            let set = |pct: u8| {
                let mut p = s.progress.lock().unwrap_or_else(|e| e.into_inner());
                p.drive = Some(drive.clone());
                p.percent = Some(pct);
            };
            set(0);
            match build_volume(&drive, &set) {
                Ok(v) => built.push(v),
                Err(e) => eprintln!("[orbit] file index: {drive} skipped: {e}"),
            }
        }
        {
            // One swap: searches use the old drives until every new one is ready.
            let mut vols = s.volumes.write().unwrap_or_else(|e| e.into_inner());
            *vols = built;
        }
        let mut p = s.progress.lock().unwrap_or_else(|e| e.into_inner());
        *p = Progress { built: true, built_at: Some(Instant::now()), ..Default::default() };
        drop(p);
        if !s.follower.swap(true, Ordering::SeqCst) {
            std::thread::spawn(follow_loop);
        }
    });
}

/// Applies each followed drive's journal every couple of seconds; reads a
/// drive again when its journal was replaced or wrapped past Orbit's position.
fn follow_loop() {
    let s = state();
    loop {
        std::thread::sleep(FOLLOW_EVERY);
        let targets: Vec<(String, u64, i64)> = s
            .volumes
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .filter(|v| v.followed)
            .map(|v| (v.drive.clone(), v.journal_id, v.next_usn))
            .collect();
        for (drive, id, usn_at) in targets {
            // Opened per poll and closed right after, so Orbit never holds a
            // volume open between polls (chkdsk and volume locks need that).
            let Ok(f) = usn::open_volume(&drive) else { continue };
            let journal = usn::query_journal(&f).ok().flatten();
            let outcome = match decide(id, usn_at, journal.as_ref()) {
                Follow::From(from) => match usn::read_journal(&f, id, from) {
                    Ok((records, next)) => Some((records, next)),
                    Err(usn::ReadError::Gone) => None,
                    Err(usn::ReadError::Other(_)) => continue,
                },
                Follow::Reindex => None,
            };
            match outcome {
                Some((records, next)) => {
                    let mut vols = s.volumes.write().unwrap_or_else(|e| e.into_inner());
                    if let Some(v) = vols.iter_mut().find(|v| v.drive == drive) {
                        for r in &records {
                            v.apply(r);
                        }
                        v.next_usn = next;
                    }
                }
                None => {
                    drop(f);
                    // Built aside; the old index answers searches until the swap.
                    if let Ok(fresh) = build_volume(&drive, &|_| {}) {
                        swap_volume(&s.volumes, fresh);
                    }
                }
            }
        }
    }
}

fn search_index(q: &Query, gen: u64) -> Option<Vec<(String, String, String, bool)>> {
    let s = state();
    let vols = s.volumes.read().unwrap_or_else(|e| e.into_inner());
    let stale = || s.gen.load(Ordering::SeqCst) != gen;
    let mut top = TopK::new(MAX_RESULTS);
    for (i, v) in vols.iter().enumerate() {
        if !v.search(i.min(255) as u8, q, &mut top, &stale) {
            return None;
        }
    }
    let mut out = Vec::new();
    for (_, vi, rec) in top.into_sorted() {
        let Some(v) = vols.get(vi as usize) else { continue };
        let (Some(path), Some(folder), Some(name)) = (v.path(rec), v.folder(rec), v.name(rec)) else { continue };
        out.push((path, name.to_string(), folder, v.is_dir(rec)));
    }
    Some(out)
}

fn search_windows(q: &Query, raw: &str, gen: u64) -> Option<Vec<(String, String, String, bool)>> {
    let s = state();
    let stale = || s.gen.load(Ordering::SeqCst) != gen;
    let found = wsearch::search(raw, 400, WSEARCH_BUDGET, &stale).unwrap_or_default();
    if stale() {
        return None;
    }
    let mut ranked: Vec<(i32, String, String, String, bool)> = found
        .into_iter()
        .filter_map(|(path, dir)| {
            let p = std::path::Path::new(&path);
            let name = p.file_name()?.to_string_lossy().into_owned();
            let folder = p.parent()?.to_string_lossy().into_owned();
            let score = q.score(&name);
            (score >= 0).then_some((score, path, name, folder, dir))
        })
        .collect();
    ranked.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.2.cmp(&b.2)));
    ranked.dedup_by(|a, b| a.1 == b.1);
    ranked.truncate(MAX_RESULTS);
    Some(ranked.into_iter().map(|(_, p, n, f, d)| (p, n, f, d)).collect())
}

fn file_search_blocking(query: String, gen: u64) -> FileSearchReply {
    let s = state();
    let query: String = query.chars().take(wsearch::MAX_QUERY_CHARS).collect();
    let q = Query::new(&query);
    let mut st = status();
    let reply = |stale, status, results| FileSearchReply { query: query.clone(), stale, status, results };
    if q.is_empty() || query.trim().chars().count() < 2 {
        return reply(false, st, Vec::new());
    }
    let found = if st.route == "index" {
        let needs_build = {
            let p = s.progress.lock().unwrap_or_else(|e| e.into_inner());
            let unfollowed = s.volumes.read().unwrap_or_else(|e| e.into_inner()).iter().any(|v| !v.followed);
            !p.building && (!p.built || (unfollowed && p.built_at.is_some_and(|t| t.elapsed() > UNFOLLOWED_REFRESH)))
        };
        if needs_build {
            start_build();
            st = status();
        }
        if st.state != "ready" {
            return reply(false, st, Vec::new());
        }
        search_index(&q, gen)
    } else if st.state == "ready" {
        search_windows(&q, query.trim(), gen)
    } else {
        return reply(false, st, Vec::new());
    };
    let Some(found) = found else { return reply(true, st, Vec::new()) };
    let paths = found.iter().map(|(p, _, _, d)| (p.clone(), *d)).collect();
    let Some(base) = store_if_current(&s.results, &s.gen, gen, paths) else { return reply(true, st, Vec::new()) };
    let out = found
        .into_iter()
        .enumerate()
        .map(|(i, (_, name, folder, is_dir))| FileResult { id: base + i as u32, name, folder, is_dir })
        .collect();
    reply(false, st, out)
}

/// Files and folders whose names match `query`, best first, at most 50. A
/// reply with `stale: true` lost to a newer query.
#[tauri::command]
pub async fn file_search(query: String) -> Result<FileSearchReply, String> {
    let gen = state().gen.fetch_add(1, Ordering::SeqCst) + 1;
    tauri::async_runtime::spawn_blocking(move || file_search_blocking(query, gen)).await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn file_search_status() -> Result<SearchStatus, String> {
    tauri::async_runtime::spawn_blocking(status).await.map_err(|e| e.to_string())
}

/// Opens result `id` from the latest reply (`reveal`: show it in File Explorer).
#[tauri::command]
pub async fn file_search_open(id: u32, reveal: bool) -> Result<(), String> {
    let path = {
        let results = state().results.lock().unwrap_or_else(|e| e.into_inner());
        results.lookup(id).map(|(p, _)| p.to_string())
    };
    let path = path.ok_or("That result is out of date. Search again.")?;
    tauri::async_runtime::spawn_blocking(move || launch::open(&path, reveal)).await.map_err(|e| e.to_string())?
}

/// For each pair of installed keyboard layouts, the characters that differ.
#[tauri::command]
pub async fn keyboard_layouts() -> Result<Vec<index::LayoutMap>, String> {
    tauri::async_runtime::spawn_blocking(|| index::layout_maps(&installed_layouts())).await.map_err(|e| e.to_string())
}

fn installed_layouts() -> Vec<(String, Vec<index::KeyChar>)> {
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyboardLayoutList, MapVirtualKeyExW, ToUnicodeEx, HKL, MAPVK_VK_TO_VSC};
    // SAFETY: first call sizes the list, second fills it.
    let n = unsafe { GetKeyboardLayoutList(None) }.max(0) as usize;
    let mut list = vec![HKL::default(); n.min(64)];
    let n = unsafe { GetKeyboardLayoutList(Some(&mut list)) }.max(0) as usize;
    list.truncate(n);
    let mut out = Vec::new();
    for hkl in list {
        let mut keys = Vec::new();
        for vk in 0x20u32..=0xFE {
            if (0x60..=0x6F).contains(&vk) {
                continue; // numeric keypad: same on every layout
            }
            for shift in [false, true] {
                let mut state = [0u8; 256];
                if shift {
                    state[0x10] = 0x80; // VK_SHIFT
                }
                let mut buf = [0u16; 8];
                // SAFETY: plain value arguments and stack buffers. Flag 4 keeps the
                // keyboard's dead-key state untouched.
                let got = unsafe {
                    let scan = MapVirtualKeyExW(vk, MAPVK_VK_TO_VSC, hkl);
                    ToUnicodeEx(vk, scan, &state, &mut buf, 4, hkl)
                };
                if got == 1 {
                    if let Some(c) = char::from_u32(u32::from(buf[0])) {
                        keys.push((vk as u16, shift, c));
                    }
                }
            }
        }
        out.push((format!("{:08X}", hkl.0 as usize as u32), keys));
    }
    out
}
