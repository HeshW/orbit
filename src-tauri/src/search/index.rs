//! The in-memory name index of one NTFS volume, and everything about it that
//! needs no Windows calls: parsing USN record buffers, applying change-journal
//! records, deciding when to read the whole volume again, building paths and
//! ranking names. All of it is unit tested with byte buffers (search/tests.rs).

use std::cmp::Reverse;
use std::collections::BinaryHeap;

/// NTFS's root folder is always file-table record 5.
pub const ROOT_RECORD: u32 = 5;
/// Records 0-23 are NTFS's own metadata files ($MFT, $LogFile, ...): never shown.
const FIRST_USER_RECORD: u32 = 24;
/// A record number past this is garbage: no real volume has 256 million records,
/// and a slot table that size would be 3 GB.
pub const MAX_RECORDS: u32 = 1 << 28;
/// Path building gives up past this many folders (a loop or broken parent chain).
pub const MAX_DEPTH: usize = 256;

const PRESENT: u8 = 1;
const DIR: u8 = 2;
const ATTR_DIRECTORY: u32 = 0x10;

pub const REASON_FILE_CREATE: u32 = 0x0000_0100;
pub const REASON_FILE_DELETE: u32 = 0x0000_0200;
pub const REASON_RENAME_OLD_NAME: u32 = 0x0000_1000;
pub const REASON_RENAME_NEW_NAME: u32 = 0x0000_2000;

/// One file or folder: parent record, where the name sits in the shared name
/// arena, and flags. 12 bytes; the name bytes are the only other cost.
#[derive(Clone, Copy, Default, Debug)]
struct Slot {
    parent: u32,
    off: u32,
    len: u16,
    flags: u8,
}

/// One USN record (V2 or V3), as FSCTL_ENUM_USN_DATA and FSCTL_READ_USN_JOURNAL
/// return them. References are cut to their low 64 bits (V3's 128-bit ids on
/// NTFS hold the same number there).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
    pub file: u64,
    pub parent: u64,
    pub usn: i64,
    pub reason: u32,
    pub attrs: u32,
    pub name: String,
}

fn u16_at(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(at..at.checked_add(2)?)?.try_into().ok()?))
}
fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at.checked_add(4)?)?.try_into().ok()?))
}
fn u64_at(b: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(b.get(at..at.checked_add(8)?)?.try_into().ok()?))
}

/// The first 8 bytes of an enum or journal reply: the next start reference or USN.
pub fn reply_next(buf: &[u8]) -> Option<u64> {
    u64_at(buf, 0)
}

/// Parses the records that follow the 8-byte header of a reply. Stops at the
/// first record whose length is 0, too short, or runs past the end. Records
/// of an unknown version, or with a name outside the record, are skipped.
pub fn parse_records(buf: &[u8]) -> Vec<Record> {
    let mut out = Vec::new();
    let mut off = 8usize;
    while let Some(len) = u32_at(buf, off) {
        let len = len as usize;
        let Some(end) = off.checked_add(len) else { break };
        if len < 60 || end > buf.len() {
            break;
        }
        let rec = &buf[off..end];
        if let Some(r) = parse_one(rec) {
            out.push(r);
        }
        off = end;
    }
    out
}

fn parse_one(rec: &[u8]) -> Option<Record> {
    let major = u16_at(rec, 4)?;
    // (file, parent, usn, reason, attrs, name length, name offset)
    let (file, parent, usn, reason, attrs, nl, no) = match major {
        2 => (u64_at(rec, 8)?, u64_at(rec, 16)?, u64_at(rec, 24)?, u32_at(rec, 40)?, u32_at(rec, 52)?, u16_at(rec, 56)?, u16_at(rec, 58)?),
        3 => (u64_at(rec, 8)?, u64_at(rec, 24)?, u64_at(rec, 40)?, u32_at(rec, 56)?, u32_at(rec, 68)?, u16_at(rec, 72)?, u16_at(rec, 74)?),
        _ => return None,
    };
    let (nl, no) = (nl as usize, no as usize);
    if nl == 0 || nl % 2 != 0 {
        return None;
    }
    let raw = rec.get(no..no.checked_add(nl)?)?;
    let units: Vec<u16> = raw.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
    Some(Record { file, parent, usn: usn as i64, reason, attrs, name: String::from_utf16_lossy(&units) })
}

/// The file-table record number in a file reference (low 48 bits), if sane.
pub fn record_of(reference: u64) -> Option<u32> {
    let n = reference & 0x0000_FFFF_FFFF_FFFF;
    (n < u64::from(MAX_RECORDS)).then_some(n as u32)
}

/// The change journal as FSCTL_QUERY_USN_JOURNAL reports it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct JournalInfo {
    pub id: u64,
    pub first_usn: i64,
    pub next_usn: i64,
    pub lowest_valid_usn: i64,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Follow {
    /// Keep reading the journal from this USN.
    From(i64),
    /// Read the whole volume again: no journal, a new journal, or it wrapped
    /// past the point Orbit last read.
    Reindex,
}

pub fn decide(saved_id: u64, saved_usn: i64, now: Option<&JournalInfo>) -> Follow {
    match now {
        None => Follow::Reindex,
        Some(j) if j.id != saved_id => Follow::Reindex,
        Some(j) if saved_usn < j.first_usn.max(j.lowest_valid_usn) => Follow::Reindex,
        Some(_) => Follow::From(saved_usn),
    }
}

/// One volume's names.
#[derive(Default)]
pub struct VolumeIndex {
    /// "C:"
    pub drive: String,
    slots: Vec<Slot>,
    names: Vec<u8>,
    dead: usize,
    count: usize,
    pub journal_id: u64,
    pub next_usn: i64,
    /// False when the volume had no journal at build time: changes are not followed.
    pub followed: bool,
}

impl VolumeIndex {
    pub fn new(drive: &str) -> Self {
        Self { drive: drive.to_string(), ..Default::default() }
    }

    pub fn len(&self) -> usize {
        self.count
    }

    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    pub fn memory_bytes(&self) -> usize {
        self.slots.capacity() * std::mem::size_of::<Slot>() + self.names.capacity()
    }

    /// Adds or replaces a record. False when it can't be stored (garbage number,
    /// arena full).
    pub fn insert(&mut self, record: u32, parent: u32, name: &str, dir: bool) -> bool {
        if record >= MAX_RECORDS || parent >= MAX_RECORDS || name.is_empty() {
            return false;
        }
        let mut cut = name.len().min(u16::MAX as usize);
        while !name.is_char_boundary(cut) {
            cut -= 1;
        }
        let bytes = &name.as_bytes()[..cut];
        let Ok(off) = u32::try_from(self.names.len()) else { return false };
        if off.checked_add(cut as u32).is_none() {
            return false;
        }
        let i = record as usize;
        if i >= self.slots.len() {
            self.slots.resize(i + 1, Slot::default());
        }
        let old = self.slots[i];
        if old.flags & PRESENT != 0 {
            self.dead += old.len as usize;
        } else {
            self.count += 1;
        }
        self.names.extend_from_slice(bytes);
        self.slots[i] = Slot { parent, off, len: cut as u16, flags: PRESENT | if dir { DIR } else { 0 } };
        true
    }

    pub fn remove(&mut self, record: u32) {
        if let Some(s) = self.slots.get_mut(record as usize) {
            if s.flags & PRESENT != 0 {
                self.dead += s.len as usize;
                self.count -= 1;
                *s = Slot::default();
            }
        }
    }

    /// Adds an enumeration record (FSCTL_ENUM_USN_DATA).
    pub fn add(&mut self, r: &Record) {
        if let (Some(f), Some(p)) = (record_of(r.file), record_of(r.parent)) {
            if f >= FIRST_USER_RECORD {
                self.insert(f, p, &r.name, r.attrs & ATTR_DIRECTORY != 0);
            }
        }
    }

    /// Applies a change-journal record: deletes remove, creates and new names
    /// (renames and moves) add or replace. Old-name records are ignored; the
    /// new-name record that follows carries the result.
    pub fn apply(&mut self, r: &Record) {
        let (Some(f), Some(p)) = (record_of(r.file), record_of(r.parent)) else { return };
        if f < FIRST_USER_RECORD {
            return;
        }
        if r.reason & REASON_FILE_DELETE != 0 {
            self.remove(f);
        } else if r.reason & (REASON_FILE_CREATE | REASON_RENAME_NEW_NAME) != 0 {
            self.insert(f, p, &r.name, r.attrs & ATTR_DIRECTORY != 0);
        }
        self.compact_if_wasteful();
    }

    /// Renames leave their old name bytes behind; rebuild the arena when more
    /// than half of it (and at least 1 MB) is dead.
    fn compact_if_wasteful(&mut self) {
        if self.dead < 1 << 20 || self.dead * 2 < self.names.len() {
            return;
        }
        let mut names = Vec::with_capacity(self.names.len() - self.dead);
        for s in self.slots.iter_mut().filter(|s| s.flags & PRESENT != 0) {
            let (a, b) = (s.off as usize, s.off as usize + s.len as usize);
            s.off = names.len() as u32;
            names.extend_from_slice(self.names.get(a..b).unwrap_or(&[]));
        }
        self.names = names;
        self.dead = 0;
    }

    fn slot(&self, record: u32) -> Option<&Slot> {
        self.slots.get(record as usize).filter(|s| s.flags & PRESENT != 0)
    }

    pub fn name(&self, record: u32) -> Option<&str> {
        let s = self.slot(record)?;
        let b = self.names.get(s.off as usize..s.off as usize + s.len as usize)?;
        std::str::from_utf8(b).ok()
    }

    pub fn is_dir(&self, record: u32) -> bool {
        self.slot(record).is_some_and(|s| s.flags & DIR != 0)
    }

    /// The full path, walking parents iteratively. None when a parent is
    /// missing, the chain loops, or it is deeper than MAX_DEPTH.
    pub fn path(&self, record: u32) -> Option<String> {
        let mut parts: Vec<&str> = Vec::new();
        let mut cur = record;
        for _ in 0..MAX_DEPTH {
            if cur == ROOT_RECORD {
                parts.reverse();
                return Some(format!("{}\\{}", self.drive, parts.join("\\")));
            }
            let s = self.slot(cur)?;
            parts.push(self.name(cur)?);
            if s.parent == cur {
                return None;
            }
            cur = s.parent;
        }
        None
    }

    /// The folder holding a record ("C:\Users\you"), or None like `path`.
    pub fn folder(&self, record: u32) -> Option<String> {
        self.path(self.slot(record)?.parent)
    }

    /// Scores every name against `q` into `top`. Returns false when `cancelled`
    /// said stop (checked every 32k names).
    pub fn search(&self, vol: u8, q: &Query, top: &mut TopK, cancelled: &dyn Fn() -> bool) -> bool {
        let mut buf = String::new();
        for (i, s) in self.slots.iter().enumerate() {
            if i % 32_768 == 0 && cancelled() {
                return false;
            }
            if s.flags & PRESENT == 0 {
                continue;
            }
            let Some(name) = self.names.get(s.off as usize..s.off as usize + s.len as usize) else { continue };
            let Ok(name) = std::str::from_utf8(name) else { continue };
            let score = q.score_with(name, &mut buf);
            if score >= 0 {
                top.push(score, vol, i as u32);
            }
        }
        true
    }
}

// ---------------------------------------------------------------- ranking

/// A lowercased query, ready to score names.
pub struct Query {
    q: String,
    /// The query's letters without spaces, when it is plain ASCII: every match
    /// tier needs them in order in the name, so names without them are skipped
    /// before any lowercasing or allocation.
    letters: Option<Vec<u8>>,
}

/// True when every byte of `letters` appears in `name` in order, ignoring ASCII case.
fn has_in_order(name: &[u8], letters: &[u8]) -> bool {
    let mut want = letters.iter();
    let Some(mut next) = want.next() else { return true };
    for b in name {
        if b.to_ascii_lowercase() == *next {
            match want.next() {
                Some(n) => next = n,
                None => return true,
            }
        }
    }
    false
}

fn is_boundary(c: char) -> bool {
    matches!(c, ' ' | '.' | '_' | '-' | '(' | ')' | '[' | ']' | ',' | '+' | '&' | '\'')
}

impl Query {
    pub fn new(q: &str) -> Self {
        let q = q.trim().to_lowercase();
        let letters = q.is_ascii().then(|| q.bytes().filter(|b| *b != b' ').collect());
        Self { q, letters }
    }

    pub fn is_empty(&self) -> bool {
        self.q.is_empty()
    }

    pub fn score(&self, name: &str) -> i32 {
        self.score_with(name, &mut String::new())
    }

    /// Exact > prefix > word start > contains > initials > in-order letters
    /// (the palette's fuzzy style), shorter names first within a tier. -1: no match.
    pub fn score_with(&self, name: &str, lower: &mut String) -> i32 {
        let q = self.q.as_str();
        if q.is_empty() {
            return -1;
        }
        lower.clear();
        if name.is_ascii() {
            if let Some(letters) = &self.letters {
                if !has_in_order(name.as_bytes(), letters) {
                    return -1;
                }
            }
            lower.extend(name.chars().map(|c| c.to_ascii_lowercase()));
        } else {
            for c in name.chars() {
                lower.extend(c.to_lowercase());
            }
        }
        let h = lower.as_str();
        let penalty = (h.len().saturating_sub(q.len()) as i32 / 2).min(400);
        if h == q {
            return 10_000;
        }
        if let Some(idx) = h.find(q) {
            if idx == 0 {
                return 8_000 - penalty;
            }
            let prev = h[..idx].chars().next_back().unwrap_or(' ');
            let pos = (idx as i32).min(400);
            if is_boundary(prev) {
                return 6_000 - pos - penalty;
            }
            return 4_000 - pos - penalty;
        }
        // Initials: "vsc" for "Visual Studio Code", "gtav" for "Grand_Theft_Auto_V".
        let mut initials = String::new();
        let mut prev_boundary = true;
        let mut prev_lower = false;
        for c in name.chars() {
            let start = prev_boundary || (c.is_uppercase() && prev_lower);
            if start && !is_boundary(c) {
                initials.extend(c.to_lowercase());
            }
            prev_boundary = is_boundary(c);
            prev_lower = c.is_lowercase();
        }
        if initials.len() >= 2 && initials.starts_with(q) {
            return 3_000 - penalty;
        }
        // In-order characters, like the palette's fuzzy() fallback.
        if q.chars().count() < 3 {
            return -1;
        }
        let mut score = 900i32;
        let mut rest = h.char_indices().peekable();
        let mut last: Option<usize> = None;
        for qc in q.chars().filter(|c| *c != ' ') {
            let mut found = None;
            for (i, c) in rest.by_ref() {
                if c == qc {
                    found = Some(i);
                    break;
                }
            }
            let i = match found {
                Some(i) => i,
                None => return -1,
            };
            if let Some(l) = last {
                let gap = h[l..i].chars().count() as i32 - 1;
                score -= gap.min(30);
            }
            last = Some(i);
        }
        (score - penalty / 4).clamp(1, 999)
    }
}

/// The best `cap` matches seen so far.
pub struct TopK {
    cap: usize,
    heap: BinaryHeap<Reverse<(i32, u8, u32)>>,
}

impl TopK {
    pub fn new(cap: usize) -> Self {
        Self { cap, heap: BinaryHeap::with_capacity(cap + 1) }
    }

    pub fn push(&mut self, score: i32, vol: u8, record: u32) {
        if self.cap == 0 {
            return;
        }
        if self.heap.len() == self.cap {
            if let Some(Reverse((worst, _, _))) = self.heap.peek() {
                if score <= *worst {
                    return;
                }
            }
        }
        self.heap.push(Reverse((score, vol, record)));
        if self.heap.len() > self.cap {
            self.heap.pop();
        }
    }

    /// Best first.
    pub fn into_sorted(self) -> Vec<(i32, u8, u32)> {
        let mut v: Vec<_> = self.heap.into_iter().map(|Reverse(x)| x).collect();
        v.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));
        v
    }
}

// ---------------------------------------------------------------- result ids

/// The paths behind the last results the palette got. The UI opens a result
/// by id; only ids from the latest reply resolve, so no arbitrary path is opened.
#[derive(Default)]
pub struct Results {
    base: u32,
    paths: Vec<(String, bool)>,
}

pub const ID_SPAN: u32 = 1_000;

impl Results {
    /// Replaces the stored results; returns the id of the first one.
    pub fn replace(&mut self, paths: Vec<(String, bool)>) -> u32 {
        self.base = self.base.wrapping_add(ID_SPAN).max(ID_SPAN);
        self.paths = paths;
        self.paths.truncate(ID_SPAN as usize);
        self.base
    }

    pub fn lookup(&self, id: u32) -> Option<(&str, bool)> {
        let i = id.checked_sub(self.base)?;
        self.paths.get(i as usize).map(|(p, d)| (p.as_str(), *d))
    }
}

// ---------------------------------------------------------------- keyboard layouts

/// One printable character a layout types for (virtual key, shift).
pub type KeyChar = (u16, bool, char);

#[derive(serde::Serialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct LayoutMap {
    pub from: String,
    pub to: String,
    /// Character typed on `from` -> the character the same key types on `to`.
    pub map: std::collections::BTreeMap<String, String>,
}

/// For every ordered pair of layouts, the characters that differ between them.
pub fn layout_maps(layouts: &[(String, Vec<KeyChar>)]) -> Vec<LayoutMap> {
    let printable = |c: char| !c.is_control() && !c.is_whitespace();
    let mut out = Vec::new();
    for (fi, (from, fkeys)) in layouts.iter().enumerate() {
        for (ti, (to, tkeys)) in layouts.iter().enumerate() {
            if fi == ti {
                continue;
            }
            let mut map = std::collections::BTreeMap::new();
            for &(vk, shift, c) in fkeys {
                if !printable(c) {
                    continue;
                }
                if let Some(&(_, _, d)) = tkeys.iter().find(|k| k.0 == vk && k.1 == shift) {
                    if printable(d) && d != c {
                        map.entry(c.to_string()).or_insert_with(|| d.to_string());
                    }
                }
            }
            if !map.is_empty() {
                out.push(LayoutMap { from: from.clone(), to: to.clone(), map });
            }
        }
    }
    out
}
