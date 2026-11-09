//! Users: Windows sessions (WTS) and the session actions.
//!
//! `list_sessions` reads every session with a signed-in user (`WTSEnumerateSessionsW` plus
//! `WTSQuerySessionInformationW`); it needs no admin rights. `session_action` disconnects,
//! signs out or sends a message box to one session. Without admin rights Windows only allows
//! these on the session Orbit runs in; other sessions answer "Access denied", which maps to
//! the usual "Try Restart as administrator." hint. The UI confirms sign out and disconnect.
//!
//! Tests never call the actions against a real session: they cover the pure mapping
//! functions (states, errors, times, validation) and a read-only `list_sessions`.

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct SessionInfo {
    pub session_id: u32,
    pub user_name: String,
    pub domain: String,
    /// "active" | "disconnected" | "connected" | "idle" | "listen" | "other".
    pub state: String,
    /// Remote client name, `None` for the console.
    pub client_name: Option<String>,
    /// Seconds since the Unix epoch.
    pub logon_time: Option<u64>,
    /// The session Orbit runs in.
    pub is_current: bool,
}

/// Title of the message box `session_action("message")` shows.
const MESSAGE_TITLE: &str = "Message from Orbit";
/// Longest message accepted, in characters.
const MAX_MESSAGE_CHARS: usize = 1000;

const DENIED: &str = "Access denied. Try Restart as administrator.";
const ENDED: &str = "That session has ended.";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Action {
    Disconnect,
    SignOut,
    Message,
}

fn parse_action(action: &str) -> Result<Action, String> {
    match action {
        "disconnect" => Ok(Action::Disconnect),
        "signout" => Ok(Action::SignOut),
        "message" => Ok(Action::Message),
        other => Err(format!("Unknown session action: {other}.")),
    }
}

/// `WTS_CONNECTSTATE_CLASS` value -> the contract's state string.
fn state_name(state: i32) -> &'static str {
    match state {
        0 => "active",       // WTSActive
        1 => "connected",    // WTSConnected
        4 => "disconnected", // WTSDisconnected
        5 => "idle",         // WTSIdle
        6 => "listen",       // WTSListen
        _ => "other",        // ConnectQuery, Shadow, Reset, Down, Init
    }
}

/// FILETIME (100 ns ticks since 1601) -> Unix seconds; `None` for 0 or before 1970.
fn filetime_to_unix(ft: i64) -> Option<u64> {
    const EPOCH_DIFF: i64 = 116_444_736_000_000_000;
    if ft <= EPOCH_DIFF {
        return None;
    }
    Some(((ft - EPOCH_DIFF) / 10_000_000) as u64)
}

/// The message to send: trimmed, not empty, at most `MAX_MESSAGE_CHARS` characters.
fn validate_message(message: Option<&str>) -> Result<String, String> {
    let text = message.unwrap_or("").trim();
    if text.is_empty() {
        return Err("Type a message first.".into());
    }
    if text.chars().count() > MAX_MESSAGE_CHARS {
        return Err(format!("Keep the message under {MAX_MESSAGE_CHARS} characters."));
    }
    Ok(text.to_string())
}

/// Win32 error code -> the short sentence the UI shows.
fn map_error(code: u32, action: Action) -> String {
    match code {
        5 => DENIED.into(), // ERROR_ACCESS_DENIED
        // ERROR_FILE_NOT_FOUND, ERROR_CTX_WINSTATION_NOT_FOUND, ERROR_CTX_WINSTATION_NAME_INVALID
        2 | 7022 | 7001 => ENDED.into(),
        // ERROR_CTX_WINSTATION_NOT_CONNECTED... the session has no screen to show a message on.
        7007 | 7041 if action == Action::Message => {
            "That session has no screen right now, so the message can't be shown.".into()
        }
        7007 | 7041 => "That session is not connected.".into(),
        _ => {
            let what = match action {
                Action::Disconnect => "disconnect the session",
                Action::SignOut => "sign out the session",
                Action::Message => "send the message",
            };
            format!("Could not {what} (error {code}).")
        }
    }
}

/// The Win32 code inside a `windows` error (FACILITY_WIN32 HRESULTs), or the raw HRESULT.
fn win32_code(err: &windows::core::Error) -> u32 {
    let hr = err.code().0 as u32;
    if (hr >> 16) & 0x1FFF == 7 {
        hr & 0xFFFF
    } else {
        hr
    }
}

mod imp {
    use super::*;
    use std::ptr::null_mut;
    use windows::core::{PCWSTR, PWSTR};
    use windows::Win32::Foundation::BOOL;
    use windows::Win32::System::RemoteDesktop::{
        ProcessIdToSessionId, WTSClientName, WTSConnectState, WTSDisconnectSession,
        WTSDomainName, WTSEnumerateSessionsW, WTSFreeMemory, WTSLogoffSession,
        WTSQuerySessionInformationW, WTSSendMessageW, WTSSessionInfo, WTSUserName, WTSINFOW,
        WTS_CONNECTSTATE_CLASS, WTS_CURRENT_SERVER_HANDLE, WTS_INFO_CLASS, WTS_SESSION_INFOW,
    };
    use windows::Win32::System::Threading::GetCurrentProcessId;
    use windows::Win32::UI::WindowsAndMessaging::{
        MB_ICONINFORMATION, MB_OK, MB_SETFOREGROUND, MESSAGEBOX_RESULT,
    };

    /// One `WTSQuerySessionInformationW` answer as raw bytes (freed before returning).
    fn query(session: u32, class: WTS_INFO_CLASS) -> Option<Vec<u8>> {
        let mut buf = PWSTR::null();
        let mut len = 0u32;
        // SAFETY: out pointers are valid; the buffer is freed with WTSFreeMemory below.
        unsafe { WTSQuerySessionInformationW(WTS_CURRENT_SERVER_HANDLE, session, class, &mut buf, &mut len) }
            .ok()?;
        if buf.is_null() {
            return None;
        }
        // SAFETY: WTS returned `len` readable bytes at `buf`.
        let bytes = unsafe { std::slice::from_raw_parts(buf.0 as *const u8, len as usize) }.to_vec();
        // SAFETY: `buf` came from WTSQuerySessionInformationW.
        unsafe { WTSFreeMemory(buf.0 as *mut _) };
        Some(bytes)
    }

    fn query_string(session: u32, class: WTS_INFO_CLASS) -> String {
        let Some(bytes) = query(session, class) else { return String::new() };
        let wide: Vec<u16> = bytes
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .take_while(|&c| c != 0)
            .collect();
        String::from_utf16_lossy(&wide).trim().to_string()
    }

    fn query_state(session: u32) -> Option<i32> {
        let bytes = query(session, WTSConnectState)?;
        let b: [u8; 4] = bytes.get(..4)?.try_into().ok()?;
        Some(i32::from_le_bytes(b))
    }

    fn query_logon_time(session: u32) -> Option<u64> {
        let bytes = query(session, WTSSessionInfo)?;
        if bytes.len() < std::mem::size_of::<WTSINFOW>() {
            return None;
        }
        // SAFETY: the buffer holds at least one WTSINFOW; read_unaligned copes with any alignment.
        let info: WTSINFOW = unsafe { std::ptr::read_unaligned(bytes.as_ptr() as *const WTSINFOW) };
        filetime_to_unix(info.LogonTime)
    }

    pub fn current_session() -> Option<u32> {
        let mut s = 0u32;
        // SAFETY: `s` is a valid out pointer.
        unsafe { ProcessIdToSessionId(GetCurrentProcessId(), &mut s) }.ok().map(|_| s)
    }

    pub fn list() -> Result<Vec<SessionInfo>, String> {
        let mut info: *mut WTS_SESSION_INFOW = null_mut();
        let mut count = 0u32;
        // SAFETY: out pointers are valid; the array is freed with WTSFreeMemory below.
        unsafe { WTSEnumerateSessionsW(WTS_CURRENT_SERVER_HANDLE, 0, 1, &mut info, &mut count) }
            .map_err(|e| match win32_code(&e) {
                5 => DENIED.to_string(),
                code => format!("Could not list the sessions (error {code})."),
            })?;
        let mut ids: Vec<(u32, WTS_CONNECTSTATE_CLASS)> = Vec::with_capacity(count as usize);
        if !info.is_null() {
            // SAFETY: WTS returned `count` entries at `info`.
            let rows = unsafe { std::slice::from_raw_parts(info, count as usize) };
            ids.extend(rows.iter().map(|r| (r.SessionId, r.State)));
            // SAFETY: `info` came from WTSEnumerateSessionsW.
            unsafe { WTSFreeMemory(info as *mut _) };
        }
        let current = current_session();
        let mut out = Vec::new();
        for (id, enum_state) in ids {
            let user = query_string(id, WTSUserName);
            if user.is_empty() {
                continue; // services (0), the RDP listener, a console nobody is signed in to
            }
            let state = query_state(id).unwrap_or(enum_state.0);
            let client = query_string(id, WTSClientName);
            out.push(SessionInfo {
                session_id: id,
                user_name: user,
                domain: query_string(id, WTSDomainName),
                state: state_name(state).to_string(),
                client_name: if client.is_empty() { None } else { Some(client) },
                logon_time: query_logon_time(id),
                is_current: current == Some(id),
            });
        }
        out.sort_by(|a, b| b.is_current.cmp(&a.is_current).then(a.session_id.cmp(&b.session_id)));
        Ok(out)
    }

    pub fn act(session_id: u32, action: Action, message: Option<String>) -> Result<(), String> {
        let wait = BOOL::from(false);
        let res = match action {
            // SAFETY: plain calls on the local server handle.
            Action::Disconnect => unsafe { WTSDisconnectSession(WTS_CURRENT_SERVER_HANDLE, session_id, wait) },
            Action::SignOut => unsafe { WTSLogoffSession(WTS_CURRENT_SERVER_HANDLE, session_id, wait) },
            Action::Message => {
                let text = message.unwrap_or_default();
                let title: Vec<u16> = MESSAGE_TITLE.encode_utf16().collect();
                let body: Vec<u16> = text.encode_utf16().collect();
                let mut response = MESSAGEBOX_RESULT(0);
                // SAFETY: title and body outlive the call; lengths are in bytes as WTS expects.
                // bWait = FALSE with timeout 0: returns at once (IDASYNC), nobody waits on a reply.
                unsafe {
                    WTSSendMessageW(
                        WTS_CURRENT_SERVER_HANDLE,
                        session_id,
                        PCWSTR(title.as_ptr()),
                        (title.len() * 2) as u32,
                        PCWSTR(body.as_ptr()),
                        (body.len() * 2) as u32,
                        MB_OK | MB_ICONINFORMATION | MB_SETFOREGROUND,
                        0,
                        &mut response,
                        wait,
                    )
                }
            }
        };
        res.map_err(|e| map_error(win32_code(&e), action))
    }
}

#[tauri::command]
pub async fn list_sessions() -> Result<Vec<SessionInfo>, String> {
    tauri::async_runtime::spawn_blocking(imp::list)
        .await
        .map_err(|_| "Could not list the sessions.".to_string())?
}

/// `action`: "disconnect" | "signout" | "message" (`message` is required then).
#[tauri::command]
pub async fn session_action(
    session_id: u32,
    action: String,
    message: Option<String>,
) -> Result<(), String> {
    let action = parse_action(&action)?;
    if session_id == 0 {
        return Err("Session 0 runs Windows services. Orbit won't change it.".into());
    }
    let message = match action {
        Action::Message => Some(validate_message(message.as_deref())?),
        _ => None,
    };
    tauri::async_runtime::spawn_blocking(move || imp::act(session_id, action, message))
        .await
        .map_err(|_| "Could not reach the session.".to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_actions() {
        assert_eq!(parse_action("disconnect"), Ok(Action::Disconnect));
        assert_eq!(parse_action("signout"), Ok(Action::SignOut));
        assert_eq!(parse_action("message"), Ok(Action::Message));
        assert!(parse_action("logoff").unwrap_err().starts_with("Unknown session action"));
        assert!(parse_action("").is_err());
    }

    #[test]
    fn names_states() {
        assert_eq!(state_name(0), "active");
        assert_eq!(state_name(1), "connected");
        assert_eq!(state_name(4), "disconnected");
        assert_eq!(state_name(5), "idle");
        assert_eq!(state_name(6), "listen");
        for other in [2, 3, 7, 8, 9, -1, 42] {
            assert_eq!(state_name(other), "other");
        }
    }

    #[test]
    fn converts_filetimes() {
        assert_eq!(filetime_to_unix(0), None);
        assert_eq!(filetime_to_unix(116_444_736_000_000_000), None);
        assert_eq!(filetime_to_unix(116_444_736_000_000_000 + 10_000_000), Some(1));
        // 2026-01-01T00:00:00Z
        assert_eq!(filetime_to_unix(134_116_992_000_000_000), Some(1_767_225_600));
    }

    #[test]
    fn maps_errors() {
        assert_eq!(map_error(5, Action::SignOut), DENIED);
        assert_eq!(map_error(5, Action::Message), DENIED);
        assert_eq!(map_error(7022, Action::Disconnect), ENDED);
        assert_eq!(map_error(2, Action::Message), ENDED);
        assert!(map_error(7007, Action::Message).contains("no screen"));
        assert_eq!(map_error(7007, Action::Disconnect), "That session is not connected.");
        assert_eq!(map_error(87, Action::SignOut), "Could not sign out the session (error 87).");
    }

    #[test]
    fn reads_win32_codes() {
        let denied = windows::core::Error::from_hresult(windows::core::HRESULT(0x8007_0005u32 as i32));
        assert_eq!(win32_code(&denied), 5);
        let other = windows::core::Error::from_hresult(windows::core::HRESULT(0x8000_4005u32 as i32));
        assert_eq!(win32_code(&other), 0x8000_4005);
    }

    #[test]
    fn validates_messages() {
        assert_eq!(validate_message(None), Err("Type a message first.".into()));
        assert_eq!(validate_message(Some("   \n ")), Err("Type a message first.".into()));
        assert_eq!(validate_message(Some("  Back in 5 \n")), Ok("Back in 5".into()));
        let long = "a".repeat(MAX_MESSAGE_CHARS + 1);
        assert!(validate_message(Some(&long)).unwrap_err().starts_with("Keep the message"));
        assert!(validate_message(Some(&"é".repeat(MAX_MESSAGE_CHARS))).is_ok());
    }

    #[test]
    fn refuses_bad_requests_before_touching_a_session() {
        // Every case returns before any WTS call: unknown action, session 0, empty message.
        let rt = |id: u32, a: &str, m: Option<&str>| {
            tauri::async_runtime::block_on(session_action(id, a.into(), m.map(String::from)))
        };
        assert!(rt(1, "reboot", None).unwrap_err().starts_with("Unknown session action"));
        assert!(rt(0, "signout", None).unwrap_err().starts_with("Session 0"));
        assert_eq!(rt(1, "message", Some("  ")), Err("Type a message first.".into()));
    }

    /// Read only: lists this PC's sessions and finds the one the test runs in.
    #[test]
    fn lists_own_session() {
        let list = imp::list().expect("list sessions");
        let current = imp::current_session().expect("own session");
        let me: Vec<_> = list.iter().filter(|s| s.is_current).collect();
        assert_eq!(me.len(), 1, "exactly one current session: {list:?}");
        assert_eq!(me[0].session_id, current);
        assert!(!me[0].user_name.is_empty());
        assert!(list.iter().all(|s| s.session_id != 0 && !s.user_name.is_empty()));
        assert!(["active", "connected", "disconnected", "idle", "listen", "other"].contains(&me[0].state.as_str()));
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        if let Some(t) = me[0].logon_time {
            assert!(t > 1_500_000_000 && t <= now + 60, "logon time {t}");
        }
    }
}
