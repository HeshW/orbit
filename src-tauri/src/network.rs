//! Network: the connection table (TCP and UDP, IPv4 and IPv6, with the
//! owning pid) and per-app network rates for the sampler.
//!
//! Connections come from `GetExtendedTcpTable(TCP_TABLE_OWNER_PID_ALL)` and
//! `GetExtendedUdpTable(UDP_TABLE_OWNER_PID)`; no admin rights needed.
//!
//! Per-app rates need administrator rights: a real-time ETW session
//! ("Orbit Network Trace (session N)", one per Windows session) with the
//! Microsoft-Windows-Kernel-Network provider runs on a background thread,
//! adds up sent and received bytes per pid, and
//! `per_app_rates()` turns the totals into bytes/sec on every sampler tick.
//! The session starts the first time the sampler asks while Orbit is elevated
//! and is stopped in `shutdown()` (ETW sessions outlive the process otherwise).

use std::collections::HashMap;
use std::ffi::c_void;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::Instant;

use serde::{Deserialize, Serialize};
use windows::core::{GUID, PCWSTR};
use windows::Win32::Foundation::{ERROR_ACCESS_DENIED, ERROR_ALREADY_EXISTS, ERROR_INSUFFICIENT_BUFFER, NO_ERROR};
use windows::Win32::NetworkManagement::IpHelper::{
    GetExtendedTcpTable, GetExtendedUdpTable, MIB_TCP6ROW_OWNER_PID, MIB_TCP6TABLE_OWNER_PID,
    MIB_TCPROW_OWNER_PID, MIB_TCPTABLE_OWNER_PID, MIB_UDP6ROW_OWNER_PID, MIB_UDP6TABLE_OWNER_PID,
    MIB_UDPROW_OWNER_PID, MIB_UDPTABLE_OWNER_PID, TCP_TABLE_OWNER_PID_ALL, UDP_TABLE_OWNER_PID,
};
use windows::Win32::Networking::WinSock::{AF_INET, AF_INET6};
use windows::Win32::System::Diagnostics::Etw::{
    CloseTrace, ControlTraceW, EnableTraceEx2, ProcessTrace, StartTraceW, CONTROLTRACE_HANDLE,
    EVENT_CONTROL_CODE_ENABLE_PROVIDER, EVENT_RECORD, EVENT_TRACE_CONTROL_STOP, EVENT_TRACE_PROPERTIES,
    EVENT_TRACE_REAL_TIME_MODE, PROCESSTRACE_HANDLE, PROCESS_TRACE_MODE_EVENT_RECORD,
    PROCESS_TRACE_MODE_REAL_TIME, WNODE_FLAG_TRACED_GUID,
};
use windows::Win32::System::RemoteDesktop::ProcessIdToSessionId;
use windows::Win32::System::Threading::GetCurrentProcessId;

use crate::model::PerAppStatus;

/// One socket. `protocol`: "tcp" | "udp"; `family`: "ipv4" | "ipv6".
/// UDP rows have no remote end and no state.
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Connection {
    pub protocol: String,
    pub family: String,
    pub local_address: String,
    pub local_port: u16,
    pub remote_address: Option<String>,
    pub remote_port: Option<u16>,
    /// TCP state in camelCase: "listen" | "established" | "timeWait" |
    /// "closeWait" | "synSent" | ...
    pub state: Option<String>,
    pub pid: u32,
}

// ---------------------------------------------------------------------------
// Connection table
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn list_connections() -> Result<Vec<Connection>, String> {
    tauri::async_runtime::spawn_blocking(list_connections_blocking)
        .await
        .map_err(|_| "Could not read the connection table.".to_string())?
}

/// Reads all four tables. A table that cannot be read is skipped; only when
/// every one fails is it an error.
pub fn list_connections_blocking() -> Result<Vec<Connection>, String> {
    let mut out = Vec::new();
    let mut failed = 0;

    match fetch_table(|buf, size| unsafe {
        GetExtendedTcpTable(buf, size, false, AF_INET.0 as u32, TCP_TABLE_OWNER_PID_ALL, 0)
    }) {
        Ok(buf) => out.extend(table_rows::<MIB_TCPROW_OWNER_PID>(&buf).iter().map(tcp4_row)),
        Err(_) => failed += 1,
    }
    match fetch_table(|buf, size| unsafe {
        GetExtendedTcpTable(buf, size, false, AF_INET6.0 as u32, TCP_TABLE_OWNER_PID_ALL, 0)
    }) {
        Ok(buf) => out.extend(table_rows::<MIB_TCP6ROW_OWNER_PID>(&buf).iter().map(tcp6_row)),
        Err(_) => failed += 1,
    }
    match fetch_table(|buf, size| unsafe {
        GetExtendedUdpTable(buf, size, false, AF_INET.0 as u32, UDP_TABLE_OWNER_PID, 0)
    }) {
        Ok(buf) => out.extend(table_rows::<MIB_UDPROW_OWNER_PID>(&buf).iter().map(udp4_row)),
        Err(_) => failed += 1,
    }
    match fetch_table(|buf, size| unsafe {
        GetExtendedUdpTable(buf, size, false, AF_INET6.0 as u32, UDP_TABLE_OWNER_PID, 0)
    }) {
        Ok(buf) => out.extend(table_rows::<MIB_UDP6ROW_OWNER_PID>(&buf).iter().map(udp6_row)),
        Err(_) => failed += 1,
    }

    if failed == 4 {
        return Err("Could not read the connection table.".into());
    }
    out.sort_by(|a, b| {
        a.local_port
            .cmp(&b.local_port)
            .then_with(|| a.protocol.cmp(&b.protocol))
            .then_with(|| a.family.cmp(&b.family))
            .then_with(|| a.pid.cmp(&b.pid))
    });
    Ok(out)
}

/// Calls an IP Helper table function until the buffer is big enough (the
/// table can grow between the size query and the read). Returns the table
/// bytes: a u32 entry count followed by the rows.
fn fetch_table(call: impl Fn(Option<*mut c_void>, *mut u32) -> u32) -> Result<Vec<u8>, u32> {
    let mut buf: Vec<u64> = Vec::new();
    let mut size: u32 = 0;
    for _ in 0..6 {
        let ptr = if buf.is_empty() { None } else { Some(buf.as_mut_ptr() as *mut c_void) };
        let rc = call(ptr, &mut size);
        if rc == NO_ERROR.0 {
            if buf.is_empty() {
                return Ok(Vec::new());
            }
            // SAFETY: `buf` owns `buf.len() * 8` initialised bytes.
            let bytes = unsafe { std::slice::from_raw_parts(buf.as_ptr() as *const u8, buf.len() * 8) };
            return Ok(bytes.to_vec());
        }
        if rc != ERROR_INSUFFICIENT_BUFFER.0 {
            return Err(rc);
        }
        // Some slack, so a few new sockets between the calls still fit.
        let words = (size as usize).div_ceil(8) + 256;
        buf = vec![0u64; words];
        size = (words * 8) as u32;
    }
    Err(ERROR_INSUFFICIENT_BUFFER.0)
}

// Every table is `dwNumEntries: u32` then the rows; all row fields are u32 or
// u8, so the rows start at offset 4.
const _: () = assert!(std::mem::offset_of!(MIB_TCPTABLE_OWNER_PID, table) == 4);
const _: () = assert!(std::mem::offset_of!(MIB_TCP6TABLE_OWNER_PID, table) == 4);
const _: () = assert!(std::mem::offset_of!(MIB_UDPTABLE_OWNER_PID, table) == 4);
const _: () = assert!(std::mem::offset_of!(MIB_UDP6TABLE_OWNER_PID, table) == 4);

/// The rows of a table buffer. Never reads past the buffer, even if the
/// count says more.
fn table_rows<T: Copy>(buf: &[u8]) -> Vec<T> {
    if buf.len() < 4 {
        return Vec::new();
    }
    let count = u32::from_ne_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
    let size = std::mem::size_of::<T>();
    let fits = (buf.len() - 4) / size;
    (0..count.min(fits))
        // SAFETY: the row lies inside `buf` (checked above); T is a plain C
        // struct of integers, so any bytes are a valid value.
        .map(|i| unsafe { std::ptr::read_unaligned(buf.as_ptr().add(4 + i * size) as *const T) })
        .collect()
}

/// Ports are stored in network byte order in the low 16 bits.
fn port(raw: u32) -> u16 {
    u16::from_be(raw as u16)
}

/// IPv4 addresses are stored in network byte order (memory order = octets).
fn ipv4(raw: u32) -> String {
    Ipv4Addr::from(raw.to_ne_bytes()).to_string()
}

/// IPv6 address text; link-local addresses keep their zone ("fe80::1%12"),
/// as Windows shows them.
fn ipv6(bytes: [u8; 16], scope: u32) -> String {
    let addr = Ipv6Addr::from(bytes);
    let link_local = bytes[0] == 0xfe && (bytes[1] & 0xc0) == 0x80;
    if link_local && scope != 0 {
        format!("{addr}%{scope}")
    } else {
        addr.to_string()
    }
}

/// MIB_TCP_STATE values to the contract's camelCase names.
fn tcp_state(state: u32) -> &'static str {
    match state {
        1 => "closed",
        2 => "listen",
        3 => "synSent",
        4 => "synReceived",
        5 => "established",
        6 => "finWait1",
        7 => "finWait2",
        8 => "closeWait",
        9 => "closing",
        10 => "lastAck",
        11 => "timeWait",
        12 => "deleteTcb",
        _ => "unknown",
    }
}

const LISTEN: u32 = 2;

fn tcp4_row(r: &MIB_TCPROW_OWNER_PID) -> Connection {
    let listening = r.dwState == LISTEN;
    Connection {
        protocol: "tcp".into(),
        family: "ipv4".into(),
        local_address: ipv4(r.dwLocalAddr),
        local_port: port(r.dwLocalPort),
        remote_address: (!listening).then(|| ipv4(r.dwRemoteAddr)),
        remote_port: (!listening).then(|| port(r.dwRemotePort)),
        state: Some(tcp_state(r.dwState).into()),
        pid: r.dwOwningPid,
    }
}

fn tcp6_row(r: &MIB_TCP6ROW_OWNER_PID) -> Connection {
    let listening = r.dwState == LISTEN;
    Connection {
        protocol: "tcp".into(),
        family: "ipv6".into(),
        local_address: ipv6(r.ucLocalAddr, r.dwLocalScopeId),
        local_port: port(r.dwLocalPort),
        remote_address: (!listening).then(|| ipv6(r.ucRemoteAddr, r.dwRemoteScopeId)),
        remote_port: (!listening).then(|| port(r.dwRemotePort)),
        state: Some(tcp_state(r.dwState).into()),
        pid: r.dwOwningPid,
    }
}

fn udp4_row(r: &MIB_UDPROW_OWNER_PID) -> Connection {
    Connection {
        protocol: "udp".into(),
        family: "ipv4".into(),
        local_address: ipv4(r.dwLocalAddr),
        local_port: port(r.dwLocalPort),
        remote_address: None,
        remote_port: None,
        state: None,
        pid: r.dwOwningPid,
    }
}

fn udp6_row(r: &MIB_UDP6ROW_OWNER_PID) -> Connection {
    Connection {
        protocol: "udp".into(),
        family: "ipv6".into(),
        local_address: ipv6(r.ucLocalAddr, r.dwLocalScopeId),
        local_port: port(r.dwLocalPort),
        remote_address: None,
        remote_port: None,
        state: None,
        pid: r.dwOwningPid,
    }
}

// ---------------------------------------------------------------------------
// Per-app status and rates
// ---------------------------------------------------------------------------

const NEEDS_ADMIN: &str = "Needs administrator rights. Use Run as admin.";

#[tauri::command]
pub fn per_app_network_status() -> PerAppStatus {
    per_app_status()
}

/// Called by the sampler every tick for `NetworkStats::per_app`. Starts the
/// trace the first time it is called while Orbit is elevated.
pub fn per_app_status() -> PerAppStatus {
    if !is_elevated() {
        return PerAppStatus { available: false, reason: Some(NEEDS_ADMIN.into()) };
    }
    ensure_started();
    match &trace().phase {
        Phase::Running => PerAppStatus { available: true, reason: None },
        Phase::Idle | Phase::Starting => PerAppStatus {
            available: false,
            reason: Some("Starting per-app network.".into()),
        },
        Phase::Failed(msg) => PerAppStatus { available: false, reason: Some(msg.clone()) },
        Phase::Stopped => PerAppStatus {
            available: false,
            reason: Some("Per-app network has stopped.".into()),
        },
    }
}

/// Called by the sampler every tick: pid -> (received, sent) bytes/sec since
/// the previous call, or `None` when per-app data is unavailable (the row
/// fields then stay null). Pids with no traffic since the last call are not
/// in the map. Must return quickly: one lock and a map swap.
pub fn per_app_rates() -> Option<HashMap<u32, (f64, f64)>> {
    if !is_elevated() {
        return None;
    }
    ensure_started();
    if trace().phase != Phase::Running {
        return None;
    }
    let mut totals = totals().lock().ok()?;
    Some(totals.take_rates(Instant::now()))
}

/// Called once when Orbit exits (RunEvent::Exit). Stops the trace session,
/// which also ends the consumer thread's `ProcessTrace`.
pub fn shutdown() {
    let (control, starting) = {
        let mut t = trace();
        t.shutdown = true;
        (std::mem::take(&mut t.control), t.phase == Phase::Starting)
    };
    if control != 0 {
        stop_session(control, &wide(session_name()));
    } else if starting {
        // The trace thread may be inside StartTraceW: stop the session by name.
        stop_session(0, &wide(session_name()));
    }
}

fn is_elevated() -> bool {
    static ELEVATED: OnceLock<bool> = OnceLock::new();
    // SAFETY: IsUserAnAdmin takes no arguments and only inspects the
    // current process token.
    *ELEVATED.get_or_init(|| unsafe { windows::Win32::UI::Shell::IsUserAnAdmin() }.as_bool())
}

// --- event parsing and aggregation (pure, unit tested) ----------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Dir {
    Sent,
    Received,
}

/// Microsoft-Windows-Kernel-Network event ids that carry a data size:
/// TCPv4 10/11, TCPv6 26/27, UDPv4 42/43, UDPv6 58/59 (sent/received).
/// Every one of them starts with `PID: u32, size: u32`. Other events
/// (connect, disconnect, retransmit, TCP copy, ...) are ignored.
fn parse_event(id: u16, data: &[u8]) -> Option<(u32, Dir, u32)> {
    let dir = match id {
        10 | 26 | 42 | 58 => Dir::Sent,
        11 | 27 | 43 | 59 => Dir::Received,
        _ => return None,
    };
    if data.len() < 8 {
        return None;
    }
    let pid = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
    let size = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
    Some((pid, dir, size))
}

/// Bytes per pid since the last `take_rates`.
#[derive(Default)]
struct Totals {
    bytes: HashMap<u32, (u64, u64)>, // pid -> (received, sent)
    since: Option<Instant>,
}

impl Totals {
    fn reset(&mut self, now: Instant) {
        self.bytes.clear();
        self.since = Some(now);
    }

    fn add(&mut self, pid: u32, dir: Dir, size: u32) {
        let entry = self.bytes.entry(pid).or_insert((0, 0));
        match dir {
            Dir::Received => entry.0 += size as u64,
            Dir::Sent => entry.1 += size as u64,
        }
    }

    /// Bytes/sec per pid over the time since the previous call, then starts
    /// a new period. The first call only starts the period.
    fn take_rates(&mut self, now: Instant) -> HashMap<u32, (f64, f64)> {
        let since = self.since.replace(now);
        let bytes = std::mem::take(&mut self.bytes);
        let Some(since) = since else { return HashMap::new() };
        let secs = now.saturating_duration_since(since).as_secs_f64().max(0.05);
        bytes
            .into_iter()
            .map(|(pid, (rx, tx))| (pid, (rx as f64 / secs, tx as f64 / secs)))
            .collect()
    }
}

fn totals() -> &'static Mutex<Totals> {
    static TOTALS: OnceLock<Mutex<Totals>> = OnceLock::new();
    TOTALS.get_or_init(|| Mutex::new(Totals::default()))
}

// --- trace session ------------------------------------------------------------

/// ETW session names are machine-wide, the single-instance lock is per
/// Windows session: one name per Windows session, so an elevated Orbit in
/// another session is never taken over. A session with this name can only be
/// left over from a crashed Orbit in this Windows session.
fn session_name() -> &'static str {
    static NAME: OnceLock<String> = OnceLock::new();
    NAME.get_or_init(|| {
        let mut id = 0u32;
        // SAFETY: plain Win32 call writing one u32.
        match unsafe { ProcessIdToSessionId(GetCurrentProcessId(), &mut id) } {
            Ok(()) => format!("Orbit Network Trace (session {id})"),
            Err(_) => "Orbit Network Trace".to_string(),
        }
    })
}
/// Microsoft-Windows-Kernel-Network.
const KERNEL_NETWORK: GUID = GUID::from_u128(0x7dd42a49_5329_4832_8dfd_43d979153a88);
const KEYWORD_IPV4: u64 = 0x10;
const KEYWORD_IPV6: u64 = 0x20;
const TRACE_LEVEL_VERBOSE: u8 = 5;

#[derive(Clone, Debug, PartialEq)]
enum Phase {
    Idle,
    Starting,
    Running,
    Failed(String),
    Stopped,
}

struct Trace {
    phase: Phase,
    /// Controller handle while the session runs, 0 otherwise.
    control: u64,
    /// Set by `shutdown()`; nothing starts after it.
    shutdown: bool,
}

static TRACE: Mutex<Trace> = Mutex::new(Trace { phase: Phase::Idle, control: 0, shutdown: false });

fn trace() -> MutexGuard<'static, Trace> {
    TRACE.lock().unwrap_or_else(|e| e.into_inner())
}

fn set_phase(phase: Phase) {
    let mut t = trace();
    if !t.shutdown {
        t.phase = phase;
    }
}

fn ensure_started() {
    {
        let mut t = trace();
        if t.phase != Phase::Idle || t.shutdown {
            return;
        }
        t.phase = Phase::Starting;
    }
    let spawned = std::thread::Builder::new().name("orbit-net-trace".into()).spawn(run_trace);
    if spawned.is_err() {
        set_phase(Phase::Failed("Could not start per-app network.".into()));
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// EVENT_TRACE_PROPERTIES followed by room for the logger and log file names.
#[repr(C)]
struct Props {
    props: EVENT_TRACE_PROPERTIES,
    logger_name: [u16; 260],
    log_file_name: [u16; 260],
}

impl Props {
    /// `for_control`: also give ControlTrace room to write back the log file name.
    fn new(for_control: bool) -> Box<Props> {
        // SAFETY: plain C structs and arrays; all-zero is valid.
        let mut p: Box<Props> = Box::new(unsafe { std::mem::zeroed() });
        p.props.Wnode.BufferSize = std::mem::size_of::<Props>() as u32;
        p.props.Wnode.Flags = WNODE_FLAG_TRACED_GUID;
        p.props.Wnode.ClientContext = 1; // QueryPerformanceCounter timestamps
        // Wnode.Guid stays zero: ETW assigns the session its own GUID.
        p.props.LogFileMode = EVENT_TRACE_REAL_TIME_MODE;
        p.props.BufferSize = 64; // KB
        p.props.MinimumBuffers = 4;
        p.props.MaximumBuffers = 32;
        p.props.FlushTimer = 1; // seconds
        p.props.LoggerNameOffset = std::mem::offset_of!(Props, logger_name) as u32;
        if for_control {
            p.props.LogFileNameOffset = std::mem::offset_of!(Props, log_file_name) as u32;
        }
        p
    }
}

fn start_error(code: u32) -> String {
    match code {
        c if c == ERROR_ACCESS_DENIED.0 => NEEDS_ADMIN.into(),
        1450 => "Windows has too many trace sessions open. Close another monitoring tool, then restart Orbit."
            .into(),
        c => format!("Per-app network could not start (error {c})."),
    }
}

/// Starts the real-time session. A session with our name left over from a
/// crashed Orbit is stopped and started again.
fn start_session(name: &[u16]) -> Result<u64, String> {
    for attempt in 0..2 {
        let mut props = Props::new(false);
        let mut handle = CONTROLTRACE_HANDLE::default();
        // SAFETY: `name` is NUL-terminated; `props` is a valid, sized buffer.
        let rc = unsafe { StartTraceW(&mut handle, PCWSTR(name.as_ptr()), &mut props.props) };
        if rc.0 == 0 {
            return Ok(handle.Value);
        }
        if rc == ERROR_ALREADY_EXISTS && attempt == 0 {
            stop_session(0, name);
            continue;
        }
        return Err(start_error(rc.0));
    }
    Err(start_error(ERROR_ALREADY_EXISTS.0))
}

/// Stops the session by handle (or by name when `control` is 0).
fn stop_session(control: u64, name: &[u16]) {
    let mut props = Props::new(true);
    // SAFETY: as in start_session.
    let _ = unsafe {
        ControlTraceW(
            CONTROLTRACE_HANDLE { Value: control },
            PCWSTR(name.as_ptr()),
            &mut props.props,
            EVENT_TRACE_CONTROL_STOP,
        )
    };
}

/// EVENT_TRACE_LOGFILEW. The `windows` crate only defines it (and OpenTraceW)
/// with the `Win32_System_Time` feature, which Orbit does not enable, so the
/// layout is declared here. The two parts Orbit never reads (EVENT_TRACE,
/// TRACE_LOGFILE_HEADER) are opaque, 8-byte aligned blocks of their x64 size.
#[repr(C)]
struct LogFileW {
    log_file_name: *mut u16,
    logger_name: *mut u16,
    current_time: i64,
    buffers_read: u32,
    process_trace_mode: u32,
    current_event: [u64; 11],  // EVENT_TRACE, 88 bytes
    logfile_header: [u64; 35], // TRACE_LOGFILE_HEADER, 280 bytes
    buffer_callback: *mut c_void,
    buffer_size: u32,
    filled: u32,
    events_lost: u32,
    event_record_callback: Option<unsafe extern "system" fn(*mut EVENT_RECORD)>,
    is_kernel_trace: u32,
    context: *mut c_void,
}

// Checked against the Windows SDK headers (sizeof / offsetof, x64).
#[cfg(target_pointer_width = "64")]
const _: () = {
    assert!(std::mem::size_of::<LogFileW>() == 448);
    assert!(std::mem::offset_of!(LogFileW, current_event) == 32);
    assert!(std::mem::offset_of!(LogFileW, logfile_header) == 120);
    assert!(std::mem::offset_of!(LogFileW, buffer_callback) == 400);
    assert!(std::mem::offset_of!(LogFileW, event_record_callback) == 424);
    assert!(std::mem::offset_of!(LogFileW, is_kernel_trace) == 432);
    assert!(std::mem::offset_of!(LogFileW, context) == 440);
};

#[link(name = "advapi32")]
extern "system" {
    fn OpenTraceW(logfile: *mut LogFileW) -> u64;
}

fn invalid_trace_handle(h: u64) -> bool {
    h == u64::MAX || h == 0xFFFF_FFFF
}

/// Runs on the ETW thread for every event of the session.
unsafe extern "system" fn on_event(record: *mut EVENT_RECORD) {
    // SAFETY: ETW passes a valid record for the duration of the callback.
    let Some(r) = (unsafe { record.as_ref() }) else { return };
    if r.EventHeader.ProviderId != KERNEL_NETWORK || r.UserData.is_null() {
        return;
    }
    // SAFETY: UserData points to UserDataLength bytes.
    let data = unsafe { std::slice::from_raw_parts(r.UserData as *const u8, r.UserDataLength as usize) };
    if let Some((pid, dir, size)) = parse_event(r.EventHeader.EventDescriptor.Id, data) {
        if let Ok(mut t) = totals().lock() {
            t.add(pid, dir, size);
        }
    }
}

/// The trace thread: start the session, enable the provider, then consume
/// events until the session stops.
fn run_trace() {
    let name = wide(session_name());
    let control = match start_session(&name) {
        Ok(h) => h,
        Err(msg) => {
            set_phase(Phase::Failed(msg));
            return;
        }
    };
    {
        // From here on shutdown() can stop the session by handle.
        let mut t = trace();
        if t.shutdown {
            drop(t);
            stop_session(control, &name);
            return;
        }
        t.control = control;
    }
    // SAFETY: valid session handle and provider GUID.
    let rc = unsafe {
        EnableTraceEx2(
            CONTROLTRACE_HANDLE { Value: control },
            &KERNEL_NETWORK,
            EVENT_CONTROL_CODE_ENABLE_PROVIDER.0,
            TRACE_LEVEL_VERBOSE,
            KEYWORD_IPV4 | KEYWORD_IPV6,
            0,
            0,
            None,
        )
    };
    if rc.0 != 0 {
        stop_owned_session(&name);
        set_phase(Phase::Failed(format!("Per-app network could not start (error {}).", rc.0)));
        return;
    }

    let mut logger = name.clone();
    // SAFETY: zeroed is a valid EVENT_TRACE_LOGFILEW; the fields set below are
    // all OpenTraceW needs for a real-time EVENT_RECORD consumer.
    let mut logfile: LogFileW = unsafe { std::mem::zeroed() };
    logfile.logger_name = logger.as_mut_ptr();
    logfile.process_trace_mode = PROCESS_TRACE_MODE_REAL_TIME | PROCESS_TRACE_MODE_EVENT_RECORD;
    logfile.event_record_callback = Some(on_event);
    // SAFETY: `logfile` and `logger` outlive the call.
    let consumer = unsafe { OpenTraceW(&mut logfile) };
    if invalid_trace_handle(consumer) {
        stop_owned_session(&name);
        let err = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
        set_phase(Phase::Failed(format!("Per-app network could not start (error {err}).")));
        return;
    }

    if let Ok(mut t) = totals().lock() {
        t.reset(Instant::now());
    }
    {
        let mut t = trace();
        if t.shutdown {
            // shutdown() has already stopped the session.
            drop(t);
            // SAFETY: handle from OpenTraceW.
            let _ = unsafe { CloseTrace(PROCESSTRACE_HANDLE { Value: consumer }) };
            return;
        }
        t.phase = Phase::Running;
    }

    // Blocks until the session is stopped (shutdown, or someone else stops it).
    // SAFETY: handle from OpenTraceW.
    let rc = unsafe { ProcessTrace(&[PROCESSTRACE_HANDLE { Value: consumer }], None, None) };
    // SAFETY: handle from OpenTraceW, closed once.
    let _ = unsafe { CloseTrace(PROCESSTRACE_HANDLE { Value: consumer }) };

    let mut t = trace();
    if t.shutdown {
        t.phase = Phase::Stopped;
    } else {
        // The handle may be stale (the session was stopped from outside), so
        // do not use it. If ProcessTrace failed with the session still up,
        // stop it by name: the name is ours alone (see session_name).
        t.control = 0;
        t.phase = Phase::Failed(format!(
            "Per-app network stopped (error {}). Restart Orbit to try again.",
            rc.0
        ));
        drop(t);
        stop_session(0, &name);
    }
}

/// On a failed start: stops the session unless shutdown() already took the
/// handle (and stopped it).
fn stop_owned_session(name: &[u16]) {
    let control = std::mem::take(&mut trace().control);
    if control != 0 {
        stop_session(control, name);
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// A table buffer: count, then the raw rows.
    fn table_bytes<T: Copy>(rows: &[T], claimed: u32) -> Vec<u8> {
        let mut buf = claimed.to_ne_bytes().to_vec();
        for r in rows {
            // SAFETY: reading the bytes of a plain C struct.
            let bytes = unsafe {
                std::slice::from_raw_parts(r as *const T as *const u8, std::mem::size_of::<T>())
            };
            buf.extend_from_slice(bytes);
        }
        buf
    }

    /// How the stack stores a port: network order in the low 16 bits.
    fn raw_port(p: u16) -> u32 {
        u32::from_ne_bytes([(p >> 8) as u8, p as u8, 0, 0])
    }

    fn raw_v4(a: [u8; 4]) -> u32 {
        u32::from_ne_bytes(a)
    }

    #[test]
    fn converts_ports_and_addresses() {
        assert_eq!(port(raw_port(3000)), 3000);
        assert_eq!(port(raw_port(443)), 443);
        assert_eq!(port(raw_port(65535)), 65535);
        assert_eq!(ipv4(raw_v4([127, 0, 0, 1])), "127.0.0.1");
        assert_eq!(ipv4(raw_v4([192, 168, 1, 24])), "192.168.1.24");
        assert_eq!(ipv4(0), "0.0.0.0");
        let mut lo = [0u8; 16];
        lo[15] = 1;
        assert_eq!(ipv6(lo, 0), "::1");
        assert_eq!(ipv6([0; 16], 0), "::");
        let mut ll = [0u8; 16];
        ll[0] = 0xfe;
        ll[1] = 0x80;
        ll[15] = 7;
        assert_eq!(ipv6(ll, 12), "fe80::7%12");
        // A global address never gets a zone.
        let g: [u8; 16] = "2a00:1450:4009:81f::200e".parse::<Ipv6Addr>().unwrap().octets();
        assert_eq!(ipv6(g, 3), "2a00:1450:4009:81f::200e");
    }

    #[test]
    fn names_tcp_states() {
        let names: Vec<&str> = (1..=12).map(tcp_state).collect();
        assert_eq!(
            names,
            [
                "closed", "listen", "synSent", "synReceived", "established", "finWait1", "finWait2",
                "closeWait", "closing", "lastAck", "timeWait", "deleteTcb"
            ]
        );
        assert_eq!(tcp_state(99), "unknown");
    }

    #[test]
    fn parses_tcp4_table() {
        let rows = [
            MIB_TCPROW_OWNER_PID {
                dwState: 2,
                dwLocalAddr: 0,
                dwLocalPort: raw_port(3000),
                dwRemoteAddr: 0,
                dwRemotePort: 0,
                dwOwningPid: 4321,
            },
            MIB_TCPROW_OWNER_PID {
                dwState: 5,
                dwLocalAddr: raw_v4([192, 168, 1, 24]),
                dwLocalPort: raw_port(52114),
                dwRemoteAddr: raw_v4([142, 250, 187, 206]),
                dwRemotePort: raw_port(443),
                dwOwningPid: 900,
            },
        ];
        let conns: Vec<Connection> =
            table_rows::<MIB_TCPROW_OWNER_PID>(&table_bytes(&rows, 2)).iter().map(tcp4_row).collect();
        assert_eq!(conns.len(), 2);
        let l = &conns[0];
        assert_eq!((l.protocol.as_str(), l.family.as_str()), ("tcp", "ipv4"));
        assert_eq!((l.local_address.as_str(), l.local_port, l.pid), ("0.0.0.0", 3000, 4321));
        assert_eq!(l.state.as_deref(), Some("listen"));
        assert!(l.remote_address.is_none() && l.remote_port.is_none(), "listeners have no remote end");
        let e = &conns[1];
        assert_eq!(e.local_address, "192.168.1.24");
        assert_eq!(e.remote_address.as_deref(), Some("142.250.187.206"));
        assert_eq!(e.remote_port, Some(443));
        assert_eq!(e.state.as_deref(), Some("established"));
    }

    #[test]
    fn parses_v6_and_udp_tables() {
        let mut lo = [0u8; 16];
        lo[15] = 1;
        let tcp6 = [MIB_TCP6ROW_OWNER_PID {
            ucLocalAddr: lo,
            dwLocalScopeId: 0,
            dwLocalPort: raw_port(5173),
            ucRemoteAddr: [0; 16],
            dwRemoteScopeId: 0,
            dwRemotePort: 0,
            dwState: 2,
            dwOwningPid: 77,
        }];
        let c = tcp6_row(&table_rows::<MIB_TCP6ROW_OWNER_PID>(&table_bytes(&tcp6, 1))[0]);
        assert_eq!((c.family.as_str(), c.local_address.as_str(), c.local_port), ("ipv6", "::1", 5173));
        assert!(c.remote_address.is_none());

        let udp4 = [MIB_UDPROW_OWNER_PID { dwLocalAddr: 0, dwLocalPort: raw_port(5353), dwOwningPid: 1200 }];
        let u = udp4_row(&table_rows::<MIB_UDPROW_OWNER_PID>(&table_bytes(&udp4, 1))[0]);
        assert_eq!((u.protocol.as_str(), u.local_port, u.pid), ("udp", 5353, 1200));
        assert!(u.state.is_none() && u.remote_address.is_none() && u.remote_port.is_none());

        let udp6 = [MIB_UDP6ROW_OWNER_PID {
            ucLocalAddr: [0; 16],
            dwLocalScopeId: 0,
            dwLocalPort: raw_port(500),
            dwOwningPid: 1300,
        }];
        let u6 = udp6_row(&table_rows::<MIB_UDP6ROW_OWNER_PID>(&table_bytes(&udp6, 1))[0]);
        assert_eq!((u6.family.as_str(), u6.local_address.as_str(), u6.local_port), ("ipv6", "::", 500));
    }

    #[test]
    fn never_reads_past_the_buffer() {
        let rows = [MIB_UDPROW_OWNER_PID { dwLocalAddr: 0, dwLocalPort: raw_port(1), dwOwningPid: 1 }];
        // The count claims 50 rows; only one is there.
        assert_eq!(table_rows::<MIB_UDPROW_OWNER_PID>(&table_bytes(&rows, 50)).len(), 1);
        assert!(table_rows::<MIB_UDPROW_OWNER_PID>(&[1, 0]).is_empty());
        assert!(table_rows::<MIB_UDPROW_OWNER_PID>(&[]).is_empty());
    }

    fn payload(pid: u32, size: u32, rest: usize) -> Vec<u8> {
        let mut v = pid.to_le_bytes().to_vec();
        v.extend_from_slice(&size.to_le_bytes());
        v.extend(std::iter::repeat(0xAB).take(rest));
        v
    }

    #[test]
    fn parses_network_events() {
        // TCPv4 send: PID, size, daddr, saddr, dport, sport, ... (36 bytes).
        assert_eq!(parse_event(10, &payload(1234, 1460, 28)), Some((1234, Dir::Sent, 1460)));
        assert_eq!(parse_event(11, &payload(1234, 900, 28)), Some((1234, Dir::Received, 900)));
        // TCPv6 (IPv6 addresses make the payload longer).
        assert_eq!(parse_event(26, &payload(55, 10, 52)), Some((55, Dir::Sent, 10)));
        assert_eq!(parse_event(27, &payload(55, 20, 52)), Some((55, Dir::Received, 20)));
        // UDPv4 / UDPv6.
        assert_eq!(parse_event(42, &payload(8, 512, 20)), Some((8, Dir::Sent, 512)));
        assert_eq!(parse_event(43, &payload(8, 64, 20)), Some((8, Dir::Received, 64)));
        assert_eq!(parse_event(58, &payload(9, 1, 44)), Some((9, Dir::Sent, 1)));
        assert_eq!(parse_event(59, &payload(9, 2, 44)), Some((9, Dir::Received, 2)));
        // Connect, disconnect, retransmit, TCP copy, UDP fail: no data size.
        for id in [12, 13, 14, 15, 16, 17, 18, 28, 34, 49] {
            assert_eq!(parse_event(id, &payload(1, 1, 28)), None, "event {id}");
        }
        // Too short to hold PID and size.
        assert_eq!(parse_event(10, &[1, 0, 0, 0, 5]), None);
    }

    #[test]
    fn aggregates_rates_per_pid() {
        let t0 = Instant::now();
        let mut t = Totals::default();
        // The first take only starts the period.
        t.add(1, Dir::Sent, 999);
        assert!(t.take_rates(t0).is_empty());

        for (pid, dir, size) in [(10, Dir::Received, 1000), (10, Dir::Received, 3000), (10, Dir::Sent, 500), (20, Dir::Sent, 2048)] {
            t.add(pid, dir, size);
        }
        let rates = t.take_rates(t0 + Duration::from_secs(2));
        assert_eq!(rates.len(), 2);
        assert_eq!(rates[&10], (2000.0, 250.0));
        assert_eq!(rates[&20], (0.0, 1024.0));

        // Nothing new: the next period is empty.
        assert!(t.take_rates(t0 + Duration::from_secs(3)).is_empty());

        // Events from parse_event feed the same totals.
        let (pid, dir, size) = parse_event(43, &payload(30, 4000, 20)).unwrap();
        t.add(pid, dir, size);
        let r = t.take_rates(t0 + Duration::from_millis(3500));
        assert_eq!(r[&30], (8000.0, 0.0));

        // reset drops what was counted before the session started.
        t.add(40, Dir::Sent, 100);
        t.reset(t0 + Duration::from_secs(10));
        assert!(t.take_rates(t0 + Duration::from_secs(11)).is_empty());
    }

    #[test]
    fn unelevated_status_asks_for_admin() {
        if is_elevated() {
            return; // covered by the elevated path, which cannot be checked here
        }
        let s = per_app_status();
        assert!(!s.available);
        assert_eq!(s.reason.as_deref(), Some(NEEDS_ADMIN));
        assert!(per_app_rates().is_none());
        assert_eq!(trace().phase, Phase::Idle, "no trace session is started unelevated");
        shutdown(); // nothing to stop; must not panic
    }

    #[test]
    fn lists_own_sockets() {
        use std::net::{TcpListener, TcpStream, UdpSocket};
        let me = std::process::id();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let lport = listener.local_addr().unwrap().port();
        let client = TcpStream::connect(("127.0.0.1", lport)).unwrap();
        let cport = client.local_addr().unwrap().port();
        let (_server_side, _) = listener.accept().unwrap();
        let udp = UdpSocket::bind("127.0.0.1:0").unwrap();
        let uport = udp.local_addr().unwrap().port();
        let v6 = TcpListener::bind("[::1]:0").ok();

        let conns = list_connections_blocking().unwrap();
        let mine: Vec<&Connection> = conns.iter().filter(|c| c.pid == me).collect();

        let listen = mine
            .iter()
            .find(|c| c.protocol == "tcp" && c.local_port == lport && c.state.as_deref() == Some("listen"))
            .expect("own TCP listener");
        assert_eq!((listen.family.as_str(), listen.local_address.as_str()), ("ipv4", "127.0.0.1"));
        assert!(listen.remote_address.is_none());

        let est = mine
            .iter()
            .find(|c| c.protocol == "tcp" && c.local_port == cport)
            .expect("own TCP client");
        assert_eq!(est.state.as_deref(), Some("established"));
        assert_eq!(est.remote_address.as_deref(), Some("127.0.0.1"));
        assert_eq!(est.remote_port, Some(lport));

        let u = mine.iter().find(|c| c.protocol == "udp" && c.local_port == uport).expect("own UDP socket");
        assert!(u.state.is_none() && u.remote_address.is_none());

        if let Some(v6) = v6 {
            let p6 = v6.local_addr().unwrap().port();
            let l6 = mine
                .iter()
                .find(|c| c.family == "ipv6" && c.local_port == p6)
                .expect("own IPv6 listener");
            assert_eq!(l6.local_address, "::1");
        }

        // Sorted by local port.
        assert!(conns.windows(2).all(|w| w[0].local_port <= w[1].local_port));
        drop(client);
    }

    /// `cargo test --lib dump_connections -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn dump_connections() {
        let t = Instant::now();
        let conns = list_connections_blocking().unwrap();
        println!("{} connections in {:?}", conns.len(), t.elapsed());
        for c in conns.iter().take(40) {
            println!("{c:?}");
        }
        println!("{:?}", per_app_status());
    }
}
