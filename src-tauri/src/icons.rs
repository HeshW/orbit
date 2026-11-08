//! Executable icons as PNG data URLs for the process table and details drawer.

use std::collections::{HashMap, HashSet};
use std::ffi::c_void;
use std::mem::size_of;
use std::sync::Mutex;

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use windows::core::PCWSTR;
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Gdi::{
    DeleteObject, GetDC, GetDIBits, GetObjectW, ReleaseDC, BITMAP, BITMAPINFO,
    BITMAPINFOHEADER, DIB_RGB_COLORS, HBITMAP, HDC, HGDIOBJ,
};
use windows::Win32::Storage::FileSystem::FILE_FLAGS_AND_ATTRIBUTES;
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED};
use windows::Win32::UI::Shell::{SHGetFileInfoW, SHFILEINFOW, SHGFI_ICON, SHGFI_LARGEICON};
use windows::Win32::UI::WindowsAndMessaging::{DestroyIcon, GetIconInfo, HICON, ICONINFO};

/// path -> data URL. Only successes are cached, so a failed lookup is retried later.
pub struct IconCache(pub Mutex<HashMap<String, String>>);

#[tauri::command]
pub async fn get_icons(
    paths: Vec<String>,
    cache: tauri::State<'_, IconCache>,
) -> Result<HashMap<String, String>, String> {
    let mut found = HashMap::new();
    let mut missing = Vec::new();
    {
        let cached = cache.inner().0.lock().unwrap_or_else(|p| p.into_inner());
        let mut seen = HashSet::new();
        for path in paths {
            if path.trim().is_empty() || !seen.insert(path.clone()) {
                continue;
            }
            match cached.get(&path) {
                Some(url) => {
                    found.insert(path, url.clone());
                }
                None => missing.push(path),
            }
        }
    }

    if missing.is_empty() {
        return Ok(found);
    }

    let fetched = tauri::async_runtime::spawn_blocking(move || {
        let _com = ComGuard::init();
        missing
            .into_iter()
            .map(|path| {
                let url = icon_data_url(&path).unwrap_or_default();
                (path, url)
            })
            .collect::<Vec<(String, String)>>()
    })
    .await
    .map_err(|e| format!("Could not load icons: {e}."))?;

    {
        let mut cached = cache.inner().0.lock().unwrap_or_else(|p| p.into_inner());
        for (path, url) in fetched {
            if !url.is_empty() {
                found.insert(path.clone(), url.clone());
                cached.insert(path, url);
            }
        }
    }
    Ok(found)
}

// ---------------------------------------------------------------------------
// RAII guards
// ---------------------------------------------------------------------------

/// Initializes COM for the current thread when needed and undoes it on drop.
/// S_FALSE (already initialized) still needs a matching CoUninitialize;
/// RPC_E_CHANGED_MODE (a different apartment is active) does not.
struct ComGuard(bool);

impl ComGuard {
    fn init() -> Self {
        let hr = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        ComGuard(hr.is_ok())
    }
}

impl Drop for ComGuard {
    fn drop(&mut self) {
        if self.0 {
            unsafe { CoUninitialize() };
        }
    }
}

struct IconGuard(HICON);

impl Drop for IconGuard {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            unsafe {
                let _ = DestroyIcon(self.0);
            }
        }
    }
}

struct BitmapGuard(HBITMAP);

impl Drop for BitmapGuard {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            unsafe {
                let _ = DeleteObject(HGDIOBJ(self.0 .0));
            }
        }
    }
}

struct ScreenDc(HDC);

impl ScreenDc {
    fn get() -> Option<Self> {
        let dc = unsafe { GetDC(HWND::default()) };
        if dc.is_invalid() {
            None
        } else {
            Some(ScreenDc(dc))
        }
    }
}

impl Drop for ScreenDc {
    fn drop(&mut self) {
        unsafe {
            ReleaseDC(HWND::default(), self.0);
        }
    }
}

// ---------------------------------------------------------------------------
// Icon extraction
// ---------------------------------------------------------------------------

/// SHGetFileInfoW fails at random (returns 0) when called on several threads
/// at once, which happens when the process table, the drawer and the
/// Performance view ask for icons together, so the call is serialized.
static SHELL_ICON_LOCK: Mutex<()> = Mutex::new(());

fn icon_data_url(path: &str) -> Option<String> {
    let wide: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
    let mut info = SHFILEINFOW::default();
    let lock = SHELL_ICON_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let ok = unsafe {
        SHGetFileInfoW(
            PCWSTR(wide.as_ptr()),
            FILE_FLAGS_AND_ATTRIBUTES(0),
            Some(&mut info as *mut SHFILEINFOW),
            size_of::<SHFILEINFOW>() as u32,
            SHGFI_ICON | SHGFI_LARGEICON,
        )
    };
    drop(lock);
    if ok == 0 || info.hIcon.is_invalid() {
        return None;
    }
    let icon = IconGuard(info.hIcon);
    let (width, height, rgba) = icon_to_rgba(icon.0)?;
    let png = encode_png(width, height, &rgba)?;
    Some(format!("data:image/png;base64,{}", STANDARD.encode(png)))
}

fn bitmap_size(bitmap: HBITMAP) -> Option<(i32, i32)> {
    let mut bm = BITMAP::default();
    let read = unsafe {
        GetObjectW(
            HGDIOBJ(bitmap.0),
            size_of::<BITMAP>() as i32,
            Some(&mut bm as *mut BITMAP as *mut c_void),
        )
    };
    if read == 0 {
        return None;
    }
    let (w, h) = (bm.bmWidth, bm.bmHeight.abs());
    if w <= 0 || h <= 0 || w > 512 || h > 1024 {
        return None;
    }
    Some((w, h))
}

/// Reads a bitmap as 32bpp top-down BGRA.
fn read_bgra(dc: HDC, bitmap: HBITMAP, width: i32, height: i32) -> Option<Vec<u8>> {
    let mut bmi = BITMAPINFO::default();
    bmi.bmiHeader.biSize = size_of::<BITMAPINFOHEADER>() as u32;
    bmi.bmiHeader.biWidth = width;
    bmi.bmiHeader.biHeight = -height;
    bmi.bmiHeader.biPlanes = 1;
    bmi.bmiHeader.biBitCount = 32;
    // biCompression stays 0 (BI_RGB) from Default.
    let mut buf = vec![0u8; width as usize * height as usize * 4];
    let lines = unsafe {
        GetDIBits(
            dc,
            bitmap,
            0,
            height as u32,
            Some(buf.as_mut_ptr() as *mut c_void),
            &mut bmi,
            DIB_RGB_COLORS,
        )
    };
    if lines == 0 {
        None
    } else {
        Some(buf)
    }
}

/// A mask pixel read at 32bpp is black (0) where the icon is opaque.
fn mask_opaque(px: &[u8]) -> bool {
    px[0] == 0 && px[1] == 0 && px[2] == 0
}

fn icon_to_rgba(icon: HICON) -> Option<(u32, u32, Vec<u8>)> {
    let mut ii = ICONINFO::default();
    unsafe { GetIconInfo(icon, &mut ii) }.ok()?;
    let color = BitmapGuard(ii.hbmColor);
    let mask = BitmapGuard(ii.hbmMask);
    let dc = ScreenDc::get()?;

    if !color.0.is_invalid() {
        let (w, h) = bitmap_size(color.0)?;
        let bgra = read_bgra(dc.0, color.0, w, h)?;
        let has_alpha = bgra.chunks_exact(4).any(|px| px[3] != 0);
        let mask_bits = if has_alpha || mask.0.is_invalid() {
            None
        } else {
            read_bgra(dc.0, mask.0, w, h)
        };

        let mut rgba = Vec::with_capacity(bgra.len());
        for (i, px) in bgra.chunks_exact(4).enumerate() {
            let alpha = if has_alpha {
                px[3]
            } else {
                match &mask_bits {
                    Some(m) => {
                        if mask_opaque(&m[i * 4..i * 4 + 4]) {
                            255
                        } else {
                            0
                        }
                    }
                    None => 255,
                }
            };
            rgba.extend_from_slice(&[px[2], px[1], px[0], alpha]);
        }
        return Some((w as u32, h as u32, rgba));
    }

    if mask.0.is_invalid() {
        return None;
    }

    // Monochrome icon: the mask holds the AND bitmap on top of the XOR bitmap.
    let (w, double_h) = bitmap_size(mask.0)?;
    let h = double_h / 2;
    if h <= 0 {
        return None;
    }
    let bits = read_bgra(dc.0, mask.0, w, double_h)?;
    let plane = w as usize * h as usize * 4;
    let (and_plane, xor_plane) = bits.split_at(plane);
    let mut rgba = Vec::with_capacity(plane);
    for (and_px, xor_px) in and_plane.chunks_exact(4).zip(xor_plane.chunks_exact(4)) {
        let white = !mask_opaque(xor_px);
        if mask_opaque(and_px) {
            let v = if white { 255 } else { 0 };
            rgba.extend_from_slice(&[v, v, v, 255]);
        } else if white {
            // "Invert screen" pixels have no PNG equivalent; draw them dark.
            rgba.extend_from_slice(&[0, 0, 0, 255]);
        } else {
            rgba.extend_from_slice(&[0, 0, 0, 0]);
        }
    }
    Some((w as u32, h as u32, rgba))
}

fn encode_png(width: u32, height: u32, rgba: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut out, width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().ok()?;
        writer.write_image_data(rgba).ok()?;
        writer.finish().ok()?;
    }
    Some(out)
}
