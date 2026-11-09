//! Small Win32 helpers used by the sampler:
//! - top-level window enumeration (app detection, titles, "not responding"),
//! - process owner names from the process token (covers SYSTEM, LOCAL SERVICE,
//!   NETWORK SERVICE, DWM-1 and similar accounts that sysinfo's user list
//!   does not include),
//! - image paths of processes Orbit cannot open (SystemProcessIdInformation),
//! - the physical core count,
//! - the Windows session of a process and the battery status.
//!
//! The few entry points are declared directly against their system DLLs with
//! the documented C signatures. Each unsafe block is short and commented.

use std::collections::HashMap;
use std::ffi::c_void;
use std::ptr::null_mut;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_MULTITHREADED,
};
use windows::Win32::UI::Shell::{IVirtualDesktopManager, VirtualDesktopManager};

type Hwnd = *mut c_void;
type Handle = *mut c_void;
type Bool = i32;

const GW_OWNER: u32 = 4;
const GWL_EXSTYLE: i32 = -20;
const WS_EX_TOOLWINDOW: i32 = 0x0000_0080;
const DWMWA_CLOAKED: u32 = 14;
/// Cloaked by the app itself (for example a UWP frame that is not shown).
const DWM_CLOAKED_APP: u32 = 0x1;
/// Cloaked by the shell: windows on another virtual desktop, and some
/// suspended UWP frames.
const DWM_CLOAKED_SHELL: u32 = 0x2;
/// Cloaked because the owner window is cloaked.
const DWM_CLOAKED_INHERITED: u32 = 0x4;
const TITLE_CAPACITY: usize = 512;
const CLASS_CAPACITY: usize = 64;

const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
const TOKEN_QUERY: u32 = 0x0008;
/// `TOKEN_INFORMATION_CLASS::TokenUser`.
const TOKEN_USER_CLASS: i32 = 1;
/// `LOGICAL_PROCESSOR_RELATIONSHIP::RelationProcessorCore`.
const RELATION_PROCESSOR_CORE: u32 = 0;

#[link(name = "user32")]
extern "system" {
    fn EnumWindows(
        callback: Option<unsafe extern "system" fn(Hwnd, isize) -> Bool>,
        lparam: isize,
    ) -> Bool;
    fn EnumChildWindows(
        parent: Hwnd,
        callback: Option<unsafe extern "system" fn(Hwnd, isize) -> Bool>,
        lparam: isize,
    ) -> Bool;
    fn IsWindowVisible(hwnd: Hwnd) -> Bool;
    fn GetWindow(hwnd: Hwnd, cmd: u32) -> Hwnd;
    fn GetWindowLongW(hwnd: Hwnd, index: i32) -> i32;
    fn GetWindowThreadProcessId(hwnd: Hwnd, process_id: *mut u32) -> u32;
    fn InternalGetWindowText(hwnd: Hwnd, buffer: *mut u16, max_count: i32) -> i32;
    fn IsHungAppWindow(hwnd: Hwnd) -> Bool;
    fn GetClassNameW(hwnd: Hwnd, class_name: *mut u16, max_count: i32) -> i32;
}

#[link(name = "dwmapi")]
extern "system" {
    fn DwmGetWindowAttribute(hwnd: Hwnd, attribute: u32, value: *mut c_void, size: u32) -> i32;
}

#[link(name = "kernel32")]
extern "system" {
    fn OpenProcess(desired_access: u32, inherit_handle: Bool, process_id: u32) -> Handle;
    fn CloseHandle(handle: Handle) -> Bool;
    fn GetLogicalProcessorInformation(
        buffer: *mut LogicalProcessorInfo,
        returned_length: *mut u32,
    ) -> Bool;
    fn QueryDosDeviceW(device_name: *const u16, target_path: *mut u16, max: u32) -> u32;
}

#[link(name = "ntdll")]
extern "system" {
    fn NtQuerySystemInformation(
        class: u32,
        information: *mut c_void,
        length: u32,
        return_length: *mut u32,
    ) -> i32;
}

#[link(name = "advapi32")]
extern "system" {
    fn OpenProcessToken(process: Handle, desired_access: u32, token: *mut Handle) -> Bool;
    fn GetTokenInformation(
        token: Handle,
        class: i32,
        information: *mut c_void,
        length: u32,
        return_length: *mut u32,
    ) -> Bool;
    fn IsValidSid(sid: *mut c_void) -> Bool;
    fn GetLengthSid(sid: *mut c_void) -> u32;
    fn LookupAccountSidW(
        system_name: *const u16,
        sid: *mut c_void,
        name: *mut u16,
        name_len: *mut u32,
        domain: *mut u16,
        domain_len: *mut u32,
        sid_use: *mut i32,
    ) -> Bool;
}

// ---------------------------------------------------------------------------
// Windows
// ---------------------------------------------------------------------------

/// The main window of a process, as Task Manager would pick it.
#[derive(Clone, Debug)]
pub struct WindowInfo {
    pub title: String,
    pub not_responding: bool,
}

/// State shared with the EnumWindows callback.
struct EnumCtx {
    map: HashMap<u32, WindowInfo>,
    /// Created on first need (a shell-cloaked window), at most once per
    /// enumeration. `Some(None)` means creation failed.
    desktops: Option<Option<IVirtualDesktopManager>>,
}

impl EnumCtx {
    /// True when `hwnd` sits on a real virtual desktop (a non-zero desktop
    /// GUID). Any COM failure counts as "no".
    fn on_virtual_desktop(&mut self, hwnd: Hwnd) -> bool {
        let manager = self.desktops.get_or_insert_with(|| {
            ensure_com();
            // SAFETY: plain COM activation; COM is initialised on this thread.
            unsafe { CoCreateInstance(&VirtualDesktopManager, None, CLSCTX_ALL) }.ok()
        });
        let Some(manager) = manager else {
            return false;
        };
        // SAFETY: `hwnd` is a live top-level window from EnumWindows.
        match unsafe { manager.GetWindowDesktopId(HWND(hwnd)) } {
            Ok(guid) => guid != windows::core::GUID::zeroed(),
            Err(_) => false,
        }
    }
}

/// Initialises COM (multithreaded) once on the calling thread. The sampler
/// thread lives as long as Orbit, so it is never uninitialised.
fn ensure_com() {
    thread_local! {
        static COM: () = {
            // SAFETY: no reserved argument; an already initialised thread
            // returns S_FALSE or RPC_E_CHANGED_MODE, both fine here.
            let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        };
    }
    COM.with(|_| {});
}

/// Returns, per pid, the first visible, unowned, titled, non-tool, non-cloaked
/// top-level window. `not_responding` is true if any such window of that
/// process is hung.
///
/// Special cases:
/// - "Ghost" windows (the stand-in dwm.exe creates for a hung window, with
///   the same title) are ignored, so dwm.exe never becomes a hung "app".
/// - Windows on another virtual desktop are shell-cloaked but still count.
/// - A UWP frame ("ApplicationFrameWindow", owned by ApplicationFrameHost.exe)
///   is credited to the app process that owns its CoreWindow child, so each
///   UWP app gets its own row and title. A frame without such a child
///   (suspended or detached) is skipped.
pub fn top_level_windows() -> HashMap<u32, WindowInfo> {
    let mut ctx = EnumCtx {
        map: HashMap::new(),
        desktops: None,
    };
    // SAFETY: EnumWindows calls `enum_proc` synchronously, only during this
    // call, and `enum_proc` casts lparam back to this exact context.
    unsafe {
        EnumWindows(Some(enum_proc), &mut ctx as *mut EnumCtx as isize);
    }
    ctx.map
}

unsafe extern "system" fn enum_proc(hwnd: Hwnd, lparam: isize) -> Bool {
    // SAFETY: lparam is the `&mut EnumCtx` passed by `top_level_windows`,
    // which outlives the enumeration.
    let ctx = &mut *(lparam as *mut EnumCtx);
    if let Some((frame_pid, title, is_uwp_frame)) = qualifying_window(hwnd, ctx) {
        let pid = if is_uwp_frame {
            match uwp_app_pid(hwnd, frame_pid) {
                Some(pid) => pid,
                None => return 1, // suspended or detached frame: not an app
            }
        } else {
            frame_pid
        };
        // SAFETY: plain query on a window handle supplied by EnumWindows.
        let hung = IsHungAppWindow(hwnd) != 0;
        ctx.map
            .entry(pid)
            .and_modify(|w| w.not_responding |= hung)
            .or_insert(WindowInfo {
                title,
                not_responding: hung,
            });
    }
    1 // continue enumeration
}

/// Window class name, or "" if it cannot be read.
///
/// # Safety
/// `hwnd` must be a window handle.
unsafe fn class_name(hwnd: Hwnd) -> String {
    let mut buffer = [0u16; CLASS_CAPACITY];
    // SAFETY: the buffer holds CLASS_CAPACITY u16s, passed as the max count.
    let len = GetClassNameW(hwnd, buffer.as_mut_ptr(), CLASS_CAPACITY as i32);
    if len <= 0 {
        return String::new();
    }
    String::from_utf16_lossy(&buffer[..(len as usize).min(CLASS_CAPACITY)])
}

/// Applies the "app window" rules; returns the owning pid, the title and
/// whether the window is a UWP frame.
///
/// # Safety
/// `hwnd` must be a window handle from EnumWindows.
unsafe fn qualifying_window(hwnd: Hwnd, ctx: &mut EnumCtx) -> Option<(u32, String, bool)> {
    if IsWindowVisible(hwnd) == 0 {
        return None;
    }
    let class = class_name(hwnd);
    if class == "Ghost" {
        // dwm.exe's copy of a hung window. The real window reports the hang.
        return None;
    }
    if !GetWindow(hwnd, GW_OWNER).is_null() {
        return None;
    }
    if GetWindowLongW(hwnd, GWL_EXSTYLE) & WS_EX_TOOLWINDOW != 0 {
        return None;
    }
    let mut cloaked: u32 = 0;
    // SAFETY: `cloaked` is a 4-byte out value matching DWMWA_CLOAKED.
    let hr = DwmGetWindowAttribute(
        hwnd,
        DWMWA_CLOAKED,
        (&mut cloaked as *mut u32).cast(),
        std::mem::size_of::<u32>() as u32,
    );
    if hr == 0 && cloaked != 0 {
        // APP: the app hid it itself (e.g. an unshown UWP frame), so reject.
        // INHERITED: the owner is cloaked, so this one is not shown either.
        if cloaked & (DWM_CLOAKED_APP | DWM_CLOAKED_INHERITED) != 0 {
            return None;
        }
        // SHELL alone: either a window on another virtual desktop (a real
        // app window, keep it) or a phantom UWP frame, which has no desktop
        // id (GUID_NULL or an error), so it stays out.
        if cloaked != DWM_CLOAKED_SHELL || !ctx.on_virtual_desktop(hwnd) {
            return None;
        }
    }

    // InternalGetWindowText never sends WM_GETTEXT, so a hung window (or our
    // own busy UI thread) cannot block the sampler.
    let mut buffer = [0u16; TITLE_CAPACITY];
    // SAFETY: the buffer holds TITLE_CAPACITY u16s, passed as the max count.
    let len = InternalGetWindowText(hwnd, buffer.as_mut_ptr(), TITLE_CAPACITY as i32);
    if len <= 0 {
        return None;
    }
    let title = String::from_utf16_lossy(&buffer[..(len as usize).min(TITLE_CAPACITY)]);
    let title = title.trim();
    if title.is_empty() {
        return None;
    }

    let mut pid: u32 = 0;
    // SAFETY: `pid` is a valid out pointer.
    GetWindowThreadProcessId(hwnd, &mut pid);
    if pid == 0 {
        return None;
    }
    Some((pid, title.to_string(), class == "ApplicationFrameWindow"))
}

/// For a UWP frame window owned by `frame_pid`, the pid of the first child
/// window that belongs to another process (the app's CoreWindow).
///
/// # Safety
/// `frame` must be a window handle.
unsafe fn uwp_app_pid(frame: Hwnd, frame_pid: u32) -> Option<u32> {
    // (frame pid, found pid)
    let mut search: (u32, u32) = (frame_pid, 0);
    // SAFETY: EnumChildWindows calls `child_proc` synchronously, and
    // `child_proc` casts lparam back to `search`, which outlives the call.
    EnumChildWindows(
        frame,
        Some(child_proc),
        &mut search as *mut (u32, u32) as isize,
    );
    (search.1 != 0).then_some(search.1)
}

unsafe extern "system" fn child_proc(hwnd: Hwnd, lparam: isize) -> Bool {
    // SAFETY: lparam is the `&mut (u32, u32)` passed by `uwp_app_pid`.
    let search = &mut *(lparam as *mut (u32, u32));
    let mut pid: u32 = 0;
    // SAFETY: `pid` is a valid out pointer.
    GetWindowThreadProcessId(hwnd, &mut pid);
    if pid != 0 && pid != search.0 {
        search.1 = pid;
        return 0; // stop
    }
    1
}

// ---------------------------------------------------------------------------
// Image paths without opening the process
// ---------------------------------------------------------------------------

/// `SystemProcessIdInformation`.
const SYSTEM_PROCESS_ID_INFORMATION_CLASS: u32 = 0x58;
const STATUS_INFO_LENGTH_MISMATCH: i32 = 0xC000_0004_u32 as i32;
/// Rebuild the drive letter map at most this often when a path cannot be
/// mapped (a drive was added).
const DEVICE_MAP_RETRY: Duration = Duration::from_secs(30);

#[repr(C)]
struct UnicodeString {
    /// Bytes, without the terminator.
    length: u16,
    maximum_length: u16,
    buffer: *mut u16,
}

#[repr(C)]
struct SystemProcessIdInformation {
    process_id: Handle,
    image_name: UnicodeString,
}

/// Looks up the image path of any process by pid, including SYSTEM and
/// service processes that cannot be opened without elevation. Windows
/// returns an NT device path, which is converted to a DOS path.
pub struct ImagePaths {
    /// (NT device name, drive) pairs, e.g. ("\Device\HarddiskVolume3", "C:").
    devices: Vec<(String, String)>,
    built: Instant,
}

impl Default for ImagePaths {
    fn default() -> Self {
        Self::new()
    }
}

impl ImagePaths {
    pub fn new() -> Self {
        Self {
            devices: dos_device_map(),
            built: Instant::now(),
        }
    }

    /// The DOS path of the process image, or `None` if Windows will not say
    /// or the path cannot be mapped to a drive.
    pub fn lookup(&mut self, pid: u32) -> Option<String> {
        if pid == 0 || pid == 4 {
            return None;
        }
        let nt = nt_image_path(pid)?;
        if let Some(path) = nt_to_dos(&nt, &self.devices) {
            return Some(path);
        }
        if self.built.elapsed() >= DEVICE_MAP_RETRY {
            self.devices = dos_device_map();
            self.built = Instant::now();
            return nt_to_dos(&nt, &self.devices);
        }
        None
    }
}

/// NT path of the image of `pid` ("\Device\HarddiskVolume3\Windows\...").
fn nt_image_path(pid: u32) -> Option<String> {
    let mut capacity: usize = 260; // u16 units
    for _ in 0..3 {
        let mut buffer = vec![0u16; capacity];
        let mut info = SystemProcessIdInformation {
            process_id: pid as usize as Handle,
            image_name: UnicodeString {
                length: 0,
                maximum_length: (capacity * 2).min(u16::MAX as usize) as u16,
                buffer: buffer.as_mut_ptr(),
            },
        };
        // SAFETY: `info` is a correctly laid out SYSTEM_PROCESS_ID_INFORMATION
        // whose ImageName buffer holds MaximumLength bytes and outlives the call.
        let status = unsafe {
            NtQuerySystemInformation(
                SYSTEM_PROCESS_ID_INFORMATION_CLASS,
                (&mut info as *mut SystemProcessIdInformation).cast(),
                std::mem::size_of::<SystemProcessIdInformation>() as u32,
                null_mut(),
            )
        };
        if status == STATUS_INFO_LENGTH_MISMATCH {
            // MaximumLength now holds the size needed, in bytes.
            let needed = (info.image_name.maximum_length as usize).div_ceil(2) + 1;
            if needed <= capacity {
                return None;
            }
            capacity = needed;
            continue;
        }
        if status < 0 {
            return None;
        }
        let len = (info.image_name.length as usize / 2).min(buffer.len());
        let path = String::from_utf16_lossy(&buffer[..len]);
        return if path.is_empty() { None } else { Some(path) };
    }
    None
}

/// Maps each drive letter to its NT device name with QueryDosDeviceW.
pub(crate) fn dos_device_map() -> Vec<(String, String)> {
    let mut out = Vec::new();
    for letter in b'A'..=b'Z' {
        let drive = format!("{}:", letter as char);
        let wide: Vec<u16> = drive.encode_utf16().chain(std::iter::once(0)).collect();
        let mut target = [0u16; 512];
        // SAFETY: `wide` is NUL-terminated; `target` holds 512 u16s.
        let len = unsafe { QueryDosDeviceW(wide.as_ptr(), target.as_mut_ptr(), target.len() as u32) };
        if len == 0 {
            continue;
        }
        // The result is a list of NUL-terminated strings; the first is current.
        let end = target.iter().position(|&c| c == 0).unwrap_or(target.len());
        let device = String::from_utf16_lossy(&target[..end]);
        if !device.is_empty() {
            out.push((device, drive));
        }
    }
    out
}

/// Converts an NT path to a DOS path using (device, drive) pairs. Also
/// handles "\??\" prefixes, "\SystemRoot\" and network paths under
/// "\Device\Mup\". Returns `None` for paths that map to no drive.
pub fn nt_to_dos(nt: &str, devices: &[(String, String)]) -> Option<String> {
    if let Some(rest) = nt.strip_prefix(r"\??\") {
        if let Some(unc) = rest.strip_prefix(r"UNC\") {
            return Some(format!(r"\\{unc}"));
        }
        return Some(rest.to_string());
    }
    if let Some(rest) = strip_prefix_ci(nt, r"\SystemRoot\") {
        let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".to_string());
        return Some(format!(r"{}\{rest}", root.trim_end_matches('\\')));
    }
    if let Some(rest) = strip_prefix_ci(nt, r"\Device\Mup\") {
        return Some(format!(r"\\{rest}"));
    }
    for (device, drive) in devices {
        if let Some(rest) = strip_prefix_ci(nt, device) {
            if rest.is_empty() {
                return Some(format!(r"{drive}\"));
            }
            if rest.starts_with('\\') {
                return Some(format!("{drive}{rest}"));
            }
        }
    }
    None
}

fn strip_prefix_ci<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    let head = s.get(..prefix.len())?;
    if head.eq_ignore_ascii_case(prefix) {
        Some(&s[prefix.len()..])
    } else {
        None
    }
}


// ---------------------------------------------------------------------------
// Process owner names
// ---------------------------------------------------------------------------

/// Closes a kernel handle on drop.
struct OwnedHandle(Handle);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: the handle was returned open by OpenProcess or
            // OpenProcessToken and is closed exactly once.
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
}

/// Resolves the account name that owns a process, caching names per SID so
/// slow account lookups happen once per account.
#[derive(Default)]
pub struct UserResolver {
    names_by_sid: HashMap<Vec<u8>, Option<String>>,
}

impl UserResolver {
    pub fn new() -> Self {
        Self::default()
    }

    /// Account name of the process owner ("Hesh", "SYSTEM", "LOCAL SERVICE"),
    /// or `None` if the process token cannot be opened.
    pub fn resolve(&mut self, pid: u32) -> Option<String> {
        // The idle and System processes have no openable token; both run as
        // LocalSystem.
        if pid == 0 || pid == 4 {
            return Some("SYSTEM".to_string());
        }
        // SAFETY: all handles are owned by OwnedHandle guards; buffers are
        // sized from the API's own size reports and outlive their use.
        unsafe { self.resolve_unchecked(pid) }
    }

    unsafe fn resolve_unchecked(&mut self, pid: u32) -> Option<String> {
        let process = OwnedHandle(OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid));
        if process.0.is_null() {
            return None;
        }
        let mut token_raw: Handle = null_mut();
        if OpenProcessToken(process.0, TOKEN_QUERY, &mut token_raw) == 0 {
            return None;
        }
        let token = OwnedHandle(token_raw);

        // Size, then read TOKEN_USER. u64 storage keeps it pointer-aligned.
        let mut needed: u32 = 0;
        GetTokenInformation(token.0, TOKEN_USER_CLASS, null_mut(), 0, &mut needed);
        if needed == 0 {
            return None;
        }
        let mut buffer: Vec<u64> = vec![0; (needed as usize).div_ceil(8)];
        let capacity = (buffer.len() * 8) as u32;
        if GetTokenInformation(
            token.0,
            TOKEN_USER_CLASS,
            buffer.as_mut_ptr().cast(),
            capacity,
            &mut needed,
        ) == 0
        {
            return None;
        }
        // TOKEN_USER starts with SID_AND_ATTRIBUTES, whose first field is the
        // SID pointer (pointing into `buffer`).
        let sid = *(buffer.as_ptr() as *const *mut c_void);
        if sid.is_null() || IsValidSid(sid) == 0 {
            return None;
        }

        let sid_len = GetLengthSid(sid) as usize;
        let key = std::slice::from_raw_parts(sid as *const u8, sid_len).to_vec();
        if let Some(cached) = self.names_by_sid.get(&key) {
            return cached.clone();
        }
        let name = lookup_account(sid);
        self.names_by_sid.insert(key, name.clone());
        name
    }
}

/// # Safety
/// `sid` must point to a valid SID.
unsafe fn lookup_account(sid: *mut c_void) -> Option<String> {
    let mut name_len: u32 = 256;
    let mut domain_len: u32 = 256;
    for _ in 0..2 {
        let mut name = vec![0u16; name_len as usize];
        let mut domain = vec![0u16; domain_len as usize];
        let mut sid_use: i32 = 0;
        let mut got_name = name_len;
        let mut got_domain = domain_len;
        // SAFETY: buffers match the lengths passed; a null system name means
        // the local machine.
        let ok = LookupAccountSidW(
            std::ptr::null(),
            sid,
            name.as_mut_ptr(),
            &mut got_name,
            domain.as_mut_ptr(),
            &mut got_domain,
            &mut sid_use,
        );
        if ok != 0 {
            let len = (got_name as usize).min(name.len());
            let value = String::from_utf16_lossy(&name[..len]);
            return if value.is_empty() { None } else { Some(value) };
        }
        // On ERROR_INSUFFICIENT_BUFFER the lengths now hold the sizes needed.
        if got_name <= name_len && got_domain <= domain_len {
            return None;
        }
        name_len = got_name.max(name_len);
        domain_len = got_domain.max(domain_len);
    }
    None
}

// ---------------------------------------------------------------------------
// CPU topology
// ---------------------------------------------------------------------------

/// `SYSTEM_LOGICAL_PROCESSOR_INFORMATION`: mask, relationship, 16-byte union.
/// Only `relationship` is read; the other fields exist for the layout.
#[repr(C)]
#[derive(Clone, Copy)]
#[allow(dead_code)]
struct LogicalProcessorInfo {
    processor_mask: usize,
    relationship: u32,
    data: [u64; 2],
}

/// Number of physical cores, or `None` if Windows will not say.
pub fn physical_core_count() -> Option<u32> {
    let item = std::mem::size_of::<LogicalProcessorInfo>();
    let mut length: u32 = 0;
    // SAFETY: sizing call with a null buffer; Windows writes the byte count.
    unsafe {
        GetLogicalProcessorInformation(null_mut(), &mut length);
    }
    if length == 0 {
        return None;
    }
    let empty = LogicalProcessorInfo {
        processor_mask: 0,
        relationship: 0,
        data: [0; 2],
    };
    let mut buffer = vec![empty; (length as usize).div_ceil(item)];
    let mut length = (buffer.len() * item) as u32;
    // SAFETY: `buffer` holds `length` bytes of correctly laid out structs.
    let ok = unsafe { GetLogicalProcessorInformation(buffer.as_mut_ptr(), &mut length) };
    if ok == 0 {
        return None;
    }
    let filled = (length as usize / item).min(buffer.len());
    let cores = buffer[..filled]
        .iter()
        .filter(|info| info.relationship == RELATION_PROCESSOR_CORE)
        .count() as u32;
    if cores == 0 {
        None
    } else {
        Some(cores)
    }
}

/// Windows session of a process (0 = services). The Idle and System
/// processes always run in session 0; `None` when Windows refuses to say.
pub fn process_session_id(pid: u32) -> Option<u32> {
    if pid == 0 || pid == 4 {
        return Some(0);
    }
    let mut session = 0u32;
    // SAFETY: `session` is a valid out pointer for the duration of the call.
    unsafe { windows::Win32::System::RemoteDesktop::ProcessIdToSessionId(pid, &mut session) }
        .ok()
        .map(|_| session)
}

/// pid -> session for every process, from one `WTSEnumerateProcessesW` call.
/// Needs no access to the processes, so it covers the SYSTEM processes that
/// `ProcessIdToSessionId` refuses without elevation. Empty on failure.
pub fn all_session_ids() -> HashMap<u32, u32> {
    use windows::Win32::System::RemoteDesktop::{
        WTSEnumerateProcessesW, WTSFreeMemory, WTS_CURRENT_SERVER_HANDLE, WTS_PROCESS_INFOW,
    };
    let mut info: *mut WTS_PROCESS_INFOW = null_mut();
    let mut count = 0u32;
    // SAFETY: level-1 enumeration of the local server; both out pointers are
    // valid for the call.
    let listed =
        unsafe { WTSEnumerateProcessesW(WTS_CURRENT_SERVER_HANDLE, 0, 1, &mut info, &mut count) };
    if listed.is_err() || info.is_null() {
        return HashMap::new();
    }
    // SAFETY: Windows wrote `count` structs at `info`; they are read before
    // the block is freed exactly once with WTSFreeMemory.
    unsafe {
        let map = std::slice::from_raw_parts(info, count as usize)
            .iter()
            .map(|p| (p.ProcessId, p.SessionId))
            .collect();
        WTSFreeMemory(info.cast());
        map
    }
}

/// Battery charge and power source, `None` on a PC without a battery.
pub fn battery_status() -> Option<crate::model::BatteryStats> {
    let mut status = windows::Win32::System::Power::SYSTEM_POWER_STATUS::default();
    // SAFETY: `status` is a valid, correctly laid out out pointer.
    unsafe { windows::Win32::System::Power::GetSystemPowerStatus(&mut status) }.ok()?;
    battery_from_status(
        status.ACLineStatus,
        status.BatteryFlag,
        status.BatteryLifePercent,
        status.BatteryLifeTime,
    )
}

/// `SYSTEM_POWER_STATUS` fields to `BatteryStats`. BatteryFlag 128 means no
/// battery and 255 unknown; 255 percent and `u32::MAX` seconds mean unknown.
fn battery_from_status(
    ac_line: u8,
    flag: u8,
    percent: u8,
    life_secs: u32,
) -> Option<crate::model::BatteryStats> {
    const NO_BATTERY: u8 = 128;
    const UNKNOWN: u8 = 255;
    const CHARGING: u8 = 8;
    if flag == UNKNOWN || flag & NO_BATTERY != 0 || percent == UNKNOWN {
        return None;
    }
    let charging = flag & CHARGING != 0;
    let ac_online = ac_line == 1;
    Some(crate::model::BatteryStats {
        percent: f64::from(percent.min(100)),
        charging,
        ac_online,
        seconds_left: (!charging && !ac_online && life_secs != u32::MAX)
            .then_some(u64::from(life_secs)),
    })
}

#[cfg(test)]
mod tests {
    use super::{all_session_ids, battery_from_status, nt_to_dos, process_session_id};

    #[test]
    fn reads_battery_flags() {
        // Desktop: no system battery.
        assert!(battery_from_status(1, 128, 255, u32::MAX).is_none());
        assert!(battery_from_status(255, 255, 255, u32::MAX).is_none());
        // On battery, 84%, 2.5 h left.
        let b = battery_from_status(0, 1, 84, 9000).unwrap();
        assert_eq!(b.percent, 84.0);
        assert!(!b.charging && !b.ac_online);
        assert_eq!(b.seconds_left, Some(9000));
        // Plugged in and charging (high + charging): no estimate.
        let b = battery_from_status(1, 1 | 8, 60, u32::MAX).unwrap();
        assert!(b.charging && b.ac_online);
        assert_eq!(b.seconds_left, None);
        // On battery but Windows has no estimate yet.
        let no_estimate = battery_from_status(0, 2, 20, u32::MAX).unwrap();
        assert_eq!(no_estimate.seconds_left, None);
    }

    #[test]
    fn reads_own_session() {
        assert_eq!(process_session_id(4), Some(0));
        // Any session (CI runners may run in session 0), but readable.
        let own = process_session_id(std::process::id());
        assert!(own.is_some());
        assert_eq!(all_session_ids().get(&std::process::id()).copied(), own);
    }

    fn devices() -> Vec<(String, String)> {
        vec![
            (r"\Device\HarddiskVolume3".to_string(), "C:".to_string()),
            (r"\Device\HarddiskVolume12".to_string(), "D:".to_string()),
        ]
    }

    #[test]
    fn maps_device_paths_to_drives() {
        let d = devices();
        assert_eq!(
            nt_to_dos(r"\Device\HarddiskVolume3\Windows\System32\spoolsv.exe", &d).as_deref(),
            Some(r"C:\Windows\System32\spoolsv.exe")
        );
        // Volume3 must not match Volume12 or Volume30.
        assert_eq!(
            nt_to_dos(r"\Device\HarddiskVolume12\Tools\a.exe", &d).as_deref(),
            Some(r"D:\Tools\a.exe")
        );
        assert_eq!(nt_to_dos(r"\Device\HarddiskVolume30\a.exe", &d), None);
        // Case-insensitive device match.
        assert_eq!(
            nt_to_dos(r"\device\harddiskvolume3\a.exe", &d).as_deref(),
            Some(r"C:\a.exe")
        );
    }

    #[test]
    fn maps_special_prefixes() {
        let d = devices();
        assert_eq!(nt_to_dos(r"\??\C:\x\y.exe", &d).as_deref(), Some(r"C:\x\y.exe"));
        assert_eq!(
            nt_to_dos(r"\??\UNC\server\share\y.exe", &d).as_deref(),
            Some(r"\\server\share\y.exe")
        );
        assert_eq!(
            nt_to_dos(r"\Device\Mup\server\share\y.exe", &d).as_deref(),
            Some(r"\\server\share\y.exe")
        );
        assert!(nt_to_dos(r"\SystemRoot\System32\smss.exe", &d)
            .unwrap()
            .ends_with(r"\System32\smss.exe"));
        assert_eq!(nt_to_dos("Registry", &d), None);
        assert_eq!(nt_to_dos("", &d), None);
    }
}
