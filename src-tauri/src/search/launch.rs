//! Opening a search result as the user, never as administrator. When Orbit is
//! elevated it asks the running desktop shell (Explorer, unelevated) to do it:
//! IShellWindows -> desktop folder view -> IShellDispatch2::ShellExecute.
//! If opening fails it shows the file in File Explorer the same way; if the
//! desktop shell can't be reached it reports an error and launches nothing.

use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::Command;

use windows::core::{Interface, BSTR, PCWSTR, VARIANT};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, IDispatch, IServiceProvider, CLSCTX_LOCAL_SERVER,
    COINIT_APARTMENTTHREADED,
};
use windows::Win32::UI::Shell::{
    IShellBrowser, IShellDispatch2, IShellFolderViewDual, IShellView, IShellWindows, IsUserAnAdmin, ShellExecuteW,
    ShellWindows, SID_STopLevelBrowser, SVGIO_BACKGROUND, SWC_DESKTOP, SWFO_NEEDDISPATCH,
};
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

const UNREACHABLE: &str = "Orbit couldn't reach File Explorer to open it as you. Open it from File Explorer instead.";

pub fn is_elevated() -> bool {
    // SAFETY: no arguments; inspects the current token only.
    unsafe { IsUserAnAdmin() }.as_bool()
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Opens `path` (or, with `reveal`, shows it selected in File Explorer).
pub fn open(path: &str, reveal: bool) -> Result<(), String> {
    if path.contains('"') || !Path::new(path).is_absolute() {
        return Err("That path is not valid.".into());
    }
    if !Path::new(path).exists() {
        return Err("That file no longer exists.".into());
    }
    let select = format!("/select,\"{path}\"");
    if is_elevated() {
        let path = path.to_string();
        // An STA thread of its own: the shell's objects are apartment-bound.
        return std::thread::spawn(move || {
            if !reveal && desktop_shell_execute(&path, "", "open").is_ok() {
                return Ok(());
            }
            desktop_shell_execute("explorer.exe", &select, "open").map_err(|_| UNREACHABLE.to_string())
        })
        .join()
        .map_err(|_| UNREACHABLE.to_string())?;
    }
    if !reveal {
        let (op, file) = (wide("open"), wide(path));
        // SAFETY: NUL-terminated strings live for the call.
        let r = unsafe { ShellExecuteW(None, PCWSTR(op.as_ptr()), PCWSTR(file.as_ptr()), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL) };
        if r.0 as isize > 32 {
            return Ok(());
        }
    }
    Command::new("explorer.exe")
        .raw_arg(&select)
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("Could not open File Explorer: {e}."))
}

/// Runs ShellExecute inside the desktop's Explorer process (the documented
/// way to start something unelevated from an elevated process).
fn desktop_shell_execute(file: &str, args: &str, op: &str) -> windows::core::Result<()> {
    // SAFETY: COM initialised for this thread and torn down before returning.
    unsafe {
        let inited = CoInitializeEx(None, COINIT_APARTMENTTHREADED).is_ok();
        let r = (|| {
            let windows: IShellWindows = CoCreateInstance(&ShellWindows, None, CLSCTX_LOCAL_SERVER)?;
            let loc = VARIANT::from(0i32); // CSIDL_DESKTOP
            let empty = VARIANT::default();
            let mut hwnd = 0i32;
            let disp: IDispatch = windows.FindWindowSW(&loc, &empty, SWC_DESKTOP, &mut hwnd, SWFO_NEEDDISPATCH)?;
            let sp: IServiceProvider = disp.cast()?;
            let browser: IShellBrowser = sp.QueryService(&SID_STopLevelBrowser)?;
            let view: IShellView = browser.QueryActiveShellView()?;
            let folder_view: IShellFolderViewDual = view.GetItemObject(SVGIO_BACKGROUND)?;
            let app: IShellDispatch2 = folder_view.Application()?.cast()?;
            app.ShellExecute(
                &BSTR::from(file),
                &VARIANT::from(args),
                &VARIANT::from(""),
                &VARIANT::from(op),
                &VARIANT::from(1i32), // SW_SHOWNORMAL
            )
        })();
        if inited {
            CoUninitialize();
        }
        r
    }
}
