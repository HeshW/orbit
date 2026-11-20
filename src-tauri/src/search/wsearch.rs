//! File search without administrator: the Windows Search index, asked over
//! OLE DB (the `Search.CollatorDSO` provider) with a read-only SQL query.
//! Time-limited and stopped early when a newer query starts.
//!
//! The shell's `search-ms:` folder was tried first: enumerating it from Rust
//! returned no items (its results arrive asynchronously), so it is not used.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use windows::core::{Interface, GUID, PCWSTR};
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED};
use windows::Win32::System::Search::{
    IAccessor, ICommandText, IDBCreateCommand, IDBCreateSession, IDBInitialize, IDataInitialize, IRowset, DBACCESSOR_ROWDATA,
    DBBINDING, DBMEMOWNER_CLIENTOWNED, DBPARAMIO_NOTPARAM, DBPART_LENGTH, DBPART_STATUS, DBPART_VALUE, DBTYPE_I4,
    DBTYPE_WSTR, HACCESSOR, MSDAINITIALIZE,
};
use windows::Win32::System::Services::{
    CloseServiceHandle, OpenSCManagerW, OpenServiceW, QueryServiceStatus, SC_MANAGER_CONNECT, SERVICE_QUERY_STATUS,
    SERVICE_RUNNING, SERVICE_STATUS,
};

/// DBGUID_DEFAULT: the provider's own SQL dialect.
const DBGUID_DEFAULT: GUID = GUID::from_u128(0xc8b521fb_5cf3_11ce_ade5_00aa0044773d);
const CONNECTION: &str = "Provider=Search.CollatorDSO;Extended Properties='Application=Windows';";
const PATH_CHARS: usize = 1024;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// True when the Windows Search service (WSearch) is running.
pub fn service_running() -> bool {
    let name = wide("WSearch");
    // SAFETY: handles are closed below; the name is NUL-terminated.
    unsafe {
        let Ok(scm) = OpenSCManagerW(PCWSTR::null(), PCWSTR::null(), SC_MANAGER_CONNECT) else { return false };
        let running = match OpenServiceW(scm, PCWSTR(name.as_ptr()), SERVICE_QUERY_STATUS) {
            Ok(svc) => {
                let mut st = SERVICE_STATUS::default();
                let ok = QueryServiceStatus(svc, &mut st).is_ok() && st.dwCurrentState == SERVICE_RUNNING;
                let _ = CloseServiceHandle(svc);
                ok
            }
            Err(_) => false,
        };
        let _ = CloseServiceHandle(scm);
        running
    }
}

/// Longest query text sent to Windows Search or the index, in characters.
pub const MAX_QUERY_CHARS: usize = 255;

/// The Windows Search SQL for names containing `q`, at most MAX_QUERY_CHARS of
/// it. Quotes are removed so the text can't end the string literal; LIKE's
/// wildcards are escaped as one-character classes ([_] [%] [[]) so they match
/// themselves.
pub fn search_sql(q: &str, limit: usize) -> String {
    let mut clean = String::new();
    for c in q.chars().filter(|c| !matches!(c, '\'' | '"')).take(MAX_QUERY_CHARS) {
        match c {
            '_' => clean.push_str("[_]"),
            '%' => clean.push_str("[%]"),
            '[' => clean.push_str("[[]"),
            c => clean.push(c),
        }
    }
    format!(
        "SELECT TOP {limit} System.ItemPathDisplay, System.FileAttributes FROM SystemIndex \
         WHERE SCOPE='file:' AND System.FileName LIKE '%{clean}%'"
    )
}

/// Paths (and folder flags) Windows Search finds for `q`, at most `limit`,
/// within `budget`. `stale` turning true stops it early.
pub fn search(q: &str, limit: usize, budget: Duration, stale: &dyn Fn() -> bool) -> Result<Vec<(String, bool)>, String> {
    let sql = search_sql(q, limit);
    let (tx, rx) = mpsc::channel::<Result<(String, bool), String>>();
    let stop = Arc::new(AtomicBool::new(false));
    // One query runs at a time (Execute can't be interrupted). A query that
    // arrives meanwhile waits; a newer one replaces it, and the replaced
    // caller's channel closes, so it returns with nothing.
    static QUEUE: std::sync::OnceLock<Arc<OneInFlight<Job>>> = std::sync::OnceLock::new();
    let queue = QUEUE.get_or_init(|| Arc::new(OneInFlight::default()));
    OneInFlight::submit(queue, Job { sql, tx, stop: stop.clone() }, run_job);
    let deadline = Instant::now() + budget;
    let mut out = Vec::new();
    loop {
        if out.len() >= limit || stale() {
            break;
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        match rx.recv_timeout(left.min(Duration::from_millis(100))) {
            Ok(Ok(item)) => out.push(item),
            Ok(Err(e)) => {
                stop.store(true, Ordering::Relaxed);
                if out.is_empty() {
                    return Err(e);
                }
                break;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    stop.store(true, Ordering::Relaxed);
    Ok(out)
}

pub struct Job {
    sql: String,
    tx: mpsc::Sender<Result<(String, bool), String>>,
    stop: Arc<AtomicBool>,
}

fn run_job(job: Job) {
    if job.stop.load(Ordering::Relaxed) {
        return;
    }
    if let Err(e) = query(&job.sql, &job.tx, &job.stop) {
        let _ = job.tx.send(Err(e.message().to_string()));
    }
}

/// Runs jobs one at a time on a worker thread. While one runs, only the latest
/// submitted job is kept; any job it replaces is dropped unrun.
pub struct OneInFlight<J> {
    state: Mutex<(bool, Option<J>)>,
}

impl<J> Default for OneInFlight<J> {
    fn default() -> Self {
        Self { state: Mutex::new((false, None)) }
    }
}

impl<J: Send + 'static> OneInFlight<J> {
    pub fn submit(this: &Arc<Self>, job: J, run: fn(J)) {
        {
            let mut st = this.state.lock().unwrap_or_else(|e| e.into_inner());
            st.1 = Some(job); // drops a job still waiting
            if st.0 {
                return;
            }
            st.0 = true;
        }
        let me = this.clone();
        std::thread::spawn(move || loop {
            let next = {
                let mut st = me.state.lock().unwrap_or_else(|e| e.into_inner());
                let next = st.1.take();
                if next.is_none() {
                    st.0 = false;
                }
                next
            };
            match next {
                Some(j) => run(j),
                None => return,
            }
        });
    }
}

#[repr(C)]
struct Row {
    path_status: u32,
    path_len: usize,
    path: [u16; PATH_CHARS],
    attr_status: u32,
    attr: i32,
}

fn binding(ordinal: usize, value: usize, len: Option<usize>, status: usize, kind: u16, max: usize) -> DBBINDING {
    // SAFETY: DBBINDING is plain data; all-zero is a valid empty binding (no type info, no object).
    let mut b: DBBINDING = unsafe { std::mem::zeroed() };
    b.iOrdinal = ordinal;
    b.obValue = value;
    b.obLength = len.unwrap_or(0);
    b.obStatus = status;
    b.dwPart = (DBPART_VALUE.0 | DBPART_STATUS.0 | if len.is_some() { DBPART_LENGTH.0 } else { 0 }) as u32;
    b.dwMemOwner = DBMEMOWNER_CLIENTOWNED.0 as u32;
    b.eParamIO = DBPARAMIO_NOTPARAM.0 as u32;
    b.cbMaxLen = max;
    b.wType = kind;
    b
}

/// OLE DB against Search.CollatorDSO: run the SQL, send each row.
fn query(sql: &str, tx: &mpsc::Sender<Result<(String, bool), String>>, stop: &AtomicBool) -> windows::core::Result<()> {
    use std::mem::offset_of;
    // SAFETY: COM is initialised for this thread and torn down at the end. The
    // row buffer matches the bindings (offsets from the repr(C) struct, value
    // length capped at the buffer), and every row handle is released.
    unsafe {
        let inited = CoInitializeEx(None, COINIT_MULTITHREADED).is_ok();
        let r = (|| {
            let init: IDataInitialize = CoCreateInstance(&MSDAINITIALIZE, None, CLSCTX_INPROC_SERVER)?;
            let conn = wide(CONNECTION);
            let mut ds = None;
            init.GetDataSource(None, CLSCTX_INPROC_SERVER.0, PCWSTR(conn.as_ptr()), &IDBInitialize::IID, &mut ds)?;
            let ds: IDBInitialize = ds.ok_or_else(windows::core::Error::empty)?.cast()?;
            ds.Initialize()?;
            let session: IDBCreateCommand = ds.cast::<IDBCreateSession>()?.CreateSession(None, &IDBCreateCommand::IID)?.cast()?;
            let cmd: ICommandText = session.CreateCommand(None, &ICommandText::IID)?.cast()?;
            let text = wide(sql);
            cmd.SetCommandText(&DBGUID_DEFAULT, PCWSTR(text.as_ptr()))?;
            let mut rs = None;
            cmd.Execute(None, &IRowset::IID, None, None, Some(&mut rs))?;
            let rowset: IRowset = rs.ok_or_else(windows::core::Error::empty)?.cast()?;
            let accessor: IAccessor = rowset.cast()?;
            let bindings = [
                binding(1, offset_of!(Row, path), Some(offset_of!(Row, path_len)), offset_of!(Row, path_status), DBTYPE_WSTR.0 as u16, PATH_CHARS * 2),
                binding(2, offset_of!(Row, attr), None, offset_of!(Row, attr_status), DBTYPE_I4.0 as u16, 4),
            ];
            let mut hacc = HACCESSOR::default();
            accessor.CreateAccessor(DBACCESSOR_ROWDATA.0 as u32, bindings.len(), bindings.as_ptr(), std::mem::size_of::<Row>(), &mut hacc, None)?;
            let mut row: Box<Row> = Box::new(std::mem::zeroed());
            loop {
                let mut handles = [0usize; 32];
                let mut ptr = handles.as_mut_ptr();
                let mut got = 0usize;
                let hr = (Interface::vtable(&rowset).GetNextRows)(Interface::as_raw(&rowset), 0, 0, handles.len() as isize, &mut got, &mut ptr);
                if hr.is_err() || got == 0 {
                    break;
                }
                let got = got.min(handles.len());
                let mut done = false;
                for &h in &handles[..got] {
                    if done || stop.load(Ordering::Relaxed) {
                        done = true;
                        break;
                    }
                    *row = std::mem::zeroed();
                    if rowset.GetData(h, hacc, (&mut *row as *mut Row).cast()).is_err() || row.path_status != 0 {
                        continue;
                    }
                    let n = (row.path_len / 2).min(PATH_CHARS);
                    let path = String::from_utf16_lossy(&row.path[..n]);
                    let dir = row.attr_status == 0 && row.attr & 0x10 != 0;
                    if !path.is_empty() && tx.send(Ok((path, dir))).is_err() {
                        done = true;
                    }
                }
                let _ = rowset.ReleaseRows(got, handles.as_ptr(), std::ptr::null(), std::ptr::null_mut(), std::ptr::null_mut());
                if done {
                    break;
                }
            }
            let _ = accessor.ReleaseAccessor(hacc, None);
            Ok(())
        })();
        if inited {
            CoUninitialize();
        }
        r
    }
}

#[cfg(test)]
mod live {
    /// Read-only: asks the real Windows Search index. Not run in CI.
    #[test]
    #[ignore]
    fn windows_search_finds_something() {
        assert!(super::service_running(), "Windows Search is not running");
        let t = std::time::Instant::now();
        let r = super::search("notepad", 20, std::time::Duration::from_secs(2), &|| false).unwrap();
        println!("{} results in {:?}: {:#?}", r.len(), t.elapsed(), r.iter().take(5).collect::<Vec<_>>());
        assert!(!r.is_empty());
        // An underscore must match itself, not be stripped or act as a wildcard.
        let q = std::env::var("ORBIT_LIVE_UNDERSCORE").unwrap_or_else(|_| "notepad++_".into());
        let u = super::search(&q, 20, std::time::Duration::from_secs(2), &|| false).unwrap();
        println!("underscore query {q:?}: {} results: {:#?}", u.len(), u.iter().take(5).collect::<Vec<_>>());
        assert!(!u.is_empty(), "no result for {q:?}");
        assert!(u.iter().all(|(p, _)| p.to_lowercase().contains(&q.to_lowercase())), "a result without {q:?}");
    }
}
