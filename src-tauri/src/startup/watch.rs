//! Watches everything the Startup list reads, so a change made by another app
//! (an "open at login" switch, Task Manager, Settings > Apps > Startup) reaches
//! the open Startup tab within about a second.
//!
//! One background thread sleeps in `WaitForMultipleObjects` on:
//! - `RegNotifyChangeKeyValue` events for the Run keys (HKCU, HKLM, HKLM 32-bit),
//!   the `StartupApproved` trees and the packaged-task state tree, and
//! - `FindFirstChangeNotificationW` handles for the two Startup folders.
//! It does no work while nothing changes. A burst of changes is collapsed (500 ms
//! quiet period); then the list is read once more and `orbit://startup-changed`
//! is emitted only if what the Startup tab shows actually differs from the last
//! read (the packaged-state tree also changes when apps save window positions).

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use tauri::{AppHandle, Emitter};
use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
use windows::Win32::Storage::FileSystem::{
    FindCloseChangeNotification, FindFirstChangeNotificationW, FindNextChangeNotification,
    FILE_NOTIFY_CHANGE_ATTRIBUTES, FILE_NOTIFY_CHANGE_FILE_NAME, FILE_NOTIFY_CHANGE_LAST_WRITE,
};
use windows::Win32::System::Registry::{
    RegNotifyChangeKeyValue, REG_NOTIFY_CHANGE_LAST_SET, REG_NOTIFY_CHANGE_NAME,
};
use windows::Win32::System::Threading::{CreateEventW, ResetEvent, WaitForMultipleObjects};

use super::*;

pub(crate) const EVENT: &str = "orbit://startup-changed";
/// How long things must stay quiet before the change is reported.
const QUIET: Duration = Duration::from_millis(500);

const APPROVED_ROOT: &str = r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved";
const PACKAGED_STATE_ROOT: &str =
    r"Software\Classes\Local Settings\Software\Microsoft\Windows\CurrentVersion\AppModel\SystemAppData";

/// What to watch.
pub(super) enum Target {
    /// A registry key (`subtree`: its subkeys too).
    Key { root: HKEY, subkey: String, subtree: bool },
    /// A folder (its direct files).
    Dir(PathBuf),
}

/// The real places the Startup tab reads.
pub(super) fn real_targets() -> Vec<Target> {
    let key = |root: HKEY, subkey: &str, subtree: bool| Target::Key { root, subkey: subkey.to_string(), subtree };
    let mut targets = vec![
        key(HKEY_CURRENT_USER, RUN_KEY, false),
        key(HKEY_LOCAL_MACHINE, RUN_KEY, false),
        key(HKEY_LOCAL_MACHINE, RUN32_KEY, false),
        // The whole StartupApproved tree: also sees a key being created for the first time.
        key(HKEY_CURRENT_USER, APPROVED_ROOT, true),
        key(HKEY_LOCAL_MACHINE, APPROVED_ROOT, true),
        key(HKEY_CURRENT_USER, PACKAGED_STATE_ROOT, true),
    ];
    for source in [Source::UserFolder, Source::CommonFolder] {
        if let Some(dir) = source.folder() {
            targets.push(Target::Dir(dir));
        }
    }
    targets
}

enum Armed {
    Key { key: RegKey, event: HANDLE, subtree: bool },
    Dir { handle: HANDLE },
}

impl Armed {
    fn handle(&self) -> HANDLE {
        match self {
            Armed::Key { event, .. } => *event,
            Armed::Dir { handle } => *handle,
        }
    }

    /// (Re)starts the notification. Windows signals the handle once per change batch.
    fn arm(&self) -> bool {
        match self {
            Armed::Key { key, event, subtree } => unsafe {
                let _ = ResetEvent(*event);
                RegNotifyChangeKeyValue(
                    key.0,
                    *subtree,
                    REG_NOTIFY_CHANGE_NAME | REG_NOTIFY_CHANGE_LAST_SET,
                    *event,
                    true,
                ) == ERROR_SUCCESS
            },
            Armed::Dir { handle } => unsafe { FindNextChangeNotification(*handle).is_ok() },
        }
    }
}

impl Drop for Armed {
    fn drop(&mut self) {
        unsafe {
            match self {
                Armed::Key { event, .. } => {
                    let _ = CloseHandle(*event);
                }
                Armed::Dir { handle } => {
                    let _ = FindCloseChangeNotification(*handle);
                }
            }
        }
    }
}

/// A set of armed notifications. Targets that cannot be opened (a key that
/// does not exist, a folder that is gone) are skipped.
pub(super) struct Watcher {
    armed: Vec<Armed>,
}

impl Watcher {
    pub(super) fn new(targets: &[Target]) -> Watcher {
        let mut armed = Vec::new();
        for t in targets {
            let item = match t {
                Target::Key { root, subkey, subtree } => {
                    let Ok(key) = RegKey::open(*root, subkey, KEY_NOTIFY | KEY_WOW64_64KEY) else { continue };
                    let Ok(event) = (unsafe { CreateEventW(None, true, false, None) }) else { continue };
                    Armed::Key { key, event, subtree: *subtree }
                }
                Target::Dir(dir) => {
                    let wide: Vec<u16> = dir.to_string_lossy().encode_utf16().chain(std::iter::once(0)).collect();
                    let Ok(handle) = (unsafe {
                        FindFirstChangeNotificationW(
                            PCWSTR(wide.as_ptr()),
                            false,
                            FILE_NOTIFY_CHANGE_FILE_NAME | FILE_NOTIFY_CHANGE_LAST_WRITE | FILE_NOTIFY_CHANGE_ATTRIBUTES,
                        )
                    }) else {
                        continue;
                    };
                    Armed::Dir { handle }
                }
            };
            if item.arm() {
                armed.push(item);
            }
        }
        Watcher { armed }
    }

    /// How many targets are being watched.
    pub(super) fn len(&self) -> usize {
        self.armed.len()
    }

    /// Waits up to `timeout` (None = forever) for one signalled target, re-arms it
    /// and returns true; false on timeout.
    fn wait_one(&mut self, timeout: Option<Duration>) -> bool {
        if self.armed.is_empty() {
            // Nothing could be watched: sleep instead of spinning.
            std::thread::sleep(timeout.unwrap_or(Duration::from_secs(3600)));
            return false;
        }
        let handles: Vec<HANDLE> = self.armed.iter().map(Armed::handle).collect();
        let ms = timeout.map_or(u32::MAX, |t| t.as_millis().min(u128::from(u32::MAX - 1)) as u32);
        let r = unsafe { WaitForMultipleObjects(&handles, false, ms) };
        let i = r.0.wrapping_sub(WAIT_OBJECT_0.0) as usize;
        if i < self.armed.len() {
            // A target that cannot re-arm (key deleted) is dropped rather than spinning.
            if !self.armed[i].arm() {
                self.armed.remove(i);
            }
            true
        } else {
            false
        }
    }

    /// Blocks until something changed, then until it has been quiet for `quiet`.
    /// `timeout` bounds the first wait; false if it passed with no change.
    pub(super) fn wait_for_change(&mut self, timeout: Option<Duration>, quiet: Duration) -> bool {
        if !self.wait_one(timeout) {
            return false;
        }
        while self.wait_one(Some(quiet)) {}
        true
    }
}

/// Hash of what the Startup tab shows, to tell a real change from noise.
fn fingerprint(list: &[StartupEntry]) -> u64 {
    let mut rows: Vec<(&str, &str, bool, Option<&str>)> = list
        .iter()
        .map(|e| (e.id.as_str(), e.command.as_str(), e.enabled, e.packaged_state.as_deref()))
        .collect();
    rows.sort();
    let mut h = DefaultHasher::new();
    rows.hash(&mut h);
    h.finish()
}

static LAST: AtomicU64 = AtomicU64::new(0);

/// Remembers what `list_startup` just returned, so the watcher only reports
/// differences from what the tab already has.
pub(super) fn note_list(list: &[StartupEntry]) {
    LAST.store(fingerprint(list), Ordering::Relaxed);
}

/// Starts the watcher thread (called once from lib.rs setup).
pub fn start(app: AppHandle) {
    let spawned = std::thread::Builder::new().name("orbit-startup-watch".into()).spawn(move || {
        let mut watcher = Watcher::new(&real_targets());
        loop {
            watcher.wait_for_change(None, QUIET);
            let mut list = collect_startup();
            list.extend(packaged::collect_packaged_startup(false));
            let now = fingerprint(&list);
            if LAST.swap(now, Ordering::Relaxed) != now {
                let _ = app.emit(EVENT, ());
            }
        }
    });
    if let Err(err) = spawned {
        eprintln!("[orbit] startup watcher failed to start: {err}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::System::Registry::{RegDeleteTreeW, HKEY_CURRENT_USER};

    /// A scratch key under HKCU\Software\OrbitTest, deleted when dropped.
    struct Scratch(String);

    impl Scratch {
        fn new(tag: &str) -> Scratch {
            let path = format!(r"Software\OrbitTest\{tag}_{}", std::process::id());
            RegKey::create(HKEY_CURRENT_USER, &path, KEY_SET_VALUE | KEY_READ).expect("scratch key");
            Scratch(path)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let sub = wide(&self.0);
            unsafe {
                let _ = RegDeleteTreeW(HKEY_CURRENT_USER, PCWSTR(sub.as_ptr()));
            }
            // Remove the shared parent too when nothing else is in it.
            let parent = wide(r"Software\OrbitTest");
            unsafe {
                let _ = windows::Win32::System::Registry::RegDeleteKeyW(HKEY_CURRENT_USER, PCWSTR(parent.as_ptr()));
            }
        }
    }

    #[test]
    fn registry_changes_are_seen_and_quiet_means_quiet() {
        let scratch = Scratch::new("watch_reg");
        let mut w = Watcher::new(&[Target::Key { root: HKEY_CURRENT_USER, subkey: scratch.0.clone(), subtree: true }]);
        assert_eq!(w.len(), 1);
        // Nothing changes: the wait times out (it blocks, it does not poll).
        assert!(!w.wait_for_change(Some(Duration::from_millis(300)), Duration::from_millis(50)));
        // A value is written from another thread: seen, and only after the quiet period.
        let path = scratch.0.clone();
        let writer = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            let key = RegKey::open(HKEY_CURRENT_USER, &path, KEY_SET_VALUE | KEY_READ).unwrap();
            // A burst of three writes counts as one change.
            for i in 0..3 {
                key.set_string("Discord", &format!("\"C:\\x.exe\" {i}")).unwrap();
                std::thread::sleep(Duration::from_millis(20));
            }
        });
        assert!(w.wait_for_change(Some(Duration::from_secs(5)), Duration::from_millis(300)));
        writer.join().unwrap();
        // The burst was swallowed whole: nothing more is pending.
        assert!(!w.wait_for_change(Some(Duration::from_millis(300)), Duration::from_millis(50)));
        // A change in a subkey is seen (subtree).
        let sub = RegKey::create(HKEY_CURRENT_USER, &format!("{}\\Child", scratch.0), KEY_SET_VALUE | KEY_READ).unwrap();
        sub.set_string("a", "b").unwrap();
        assert!(w.wait_for_change(Some(Duration::from_secs(5)), Duration::from_millis(50)));
    }

    #[test]
    fn folder_changes_are_seen() {
        let dir = std::env::temp_dir().join(format!("orbit-watch-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut w = Watcher::new(&[Target::Dir(dir.clone())]);
        assert_eq!(w.len(), 1);
        assert!(!w.wait_for_change(Some(Duration::from_millis(300)), Duration::from_millis(50)));
        let file = dir.join("Test.lnk");
        let f2 = file.clone();
        let writer = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            std::fs::write(&f2, b"x").unwrap();
        });
        assert!(w.wait_for_change(Some(Duration::from_secs(5)), Duration::from_millis(100)));
        writer.join().unwrap();
        // Removing it is a change too.
        std::fs::remove_file(&file).unwrap();
        assert!(w.wait_for_change(Some(Duration::from_secs(5)), Duration::from_millis(100)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_targets_are_skipped() {
        let w = Watcher::new(&[
            Target::Key { root: HKEY_CURRENT_USER, subkey: r"Software\OrbitTest\does_not_exist_xyz".into(), subtree: false },
            Target::Dir(PathBuf::from(r"C:\orbit-no-such-folder-xyz")),
        ]);
        assert_eq!(w.len(), 0);
    }

    #[test]
    fn the_real_targets_open_without_admin() {
        // Read only: just arms notifications on the real keys and folders.
        let w = Watcher::new(&real_targets());
        assert!(w.len() >= 4, "only {} targets armed", w.len());
    }

    #[test]
    fn fingerprint_ignores_order_and_catches_changes() {
        let e = |id: &str, enabled: bool| StartupEntry {
            id: id.into(),
            name: id.into(),
            command: String::new(),
            location: String::new(),
            enabled,
            path: None,
            needs_admin: false,
            impact: None,
            impact_measured_at: None,
            packaged_state: None,
        };
        let a = [e("a", true), e("b", false)];
        let b = [e("b", false), e("a", true)];
        assert_eq!(fingerprint(&a), fingerprint(&b));
        assert_ne!(fingerprint(&a), fingerprint(&[e("a", false), e("b", false)]));
        assert_ne!(fingerprint(&a), fingerprint(&a[..1]));
    }
}
