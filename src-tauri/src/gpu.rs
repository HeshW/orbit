//! Graphics adapters: model name, dedicated memory and driver version, read
//! once through DXGI (`EnumAdapters1`, no admin) and cached for the run.
//!
//! Each adapter carries its LUID in the same form pdh.rs gives the GPU
//! counters (`luid_key`), so the UI puts the right name on the right load
//! line (Snapshot `gpu.adapters`). Software adapters (Microsoft Basic Render
//! Driver, remote display) are left out. The driver version comes from
//! `CheckInterfaceSupport(IDXGIDevice)`: the user-mode driver version, the
//! same four numbers Device Manager shows ("32.0.15.9621").
//!
//! DXGI can list one card twice (a virtual display driver such as
//! SuperDisplay adds a second LUID with the card's name). `get_gpu_info`
//! drops such a twin when the other copy has GPU counters (`visible`).

use std::sync::OnceLock;

use serde::Serialize;
use tauri::State;
use windows::core::Interface;
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, IDXGIDevice, IDXGIFactory1, DXGI_ADAPTER_FLAG_SOFTWARE,
};

/// Microsoft's PCI vendor id: Basic Render Driver, remote and virtual adapters.
const VENDOR_MICROSOFT: u32 = 0x1414;
/// More than any real PC has; stops a broken enumerator.
const MAX_ADAPTERS: u32 = 16;

#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GpuAdapter {
    /// "0x00000000_0x0000d1a5" (`luid_key`), matches Snapshot `gpu.adapters[].luid`.
    pub luid: String,
    /// "NVIDIA GeForce RTX 4060".
    pub name: String,
    /// "NVIDIA", "AMD", "Intel", ... `None` for an unknown PCI vendor id.
    pub vendor: Option<String>,
    /// Bytes of video memory on the card (small or 0 for integrated graphics).
    pub dedicated_memory: u64,
    /// Bytes of system memory the adapter may borrow.
    pub shared_memory: u64,
    /// "32.0.15.9621", `None` when the driver does not say.
    pub driver_version: Option<String>,
}

/// The one LUID format both sides use: high part, then low part, 8 lowercase
/// hex digits each.
pub fn luid_key(high: u32, low: u32) -> String {
    format!("0x{high:08x}_0x{low:08x}")
}

/// The four 16-bit parts of a UMD version, as Windows writes them.
pub fn driver_version_text(v: i64) -> Option<String> {
    let v = v as u64;
    if v == 0 {
        return None;
    }
    Some(format!("{}.{}.{}.{}", (v >> 48) & 0xffff, (v >> 32) & 0xffff, (v >> 16) & 0xffff, v & 0xffff))
}

pub fn vendor_name(id: u32) -> Option<&'static str> {
    Some(match id {
        0x10de => "NVIDIA",
        0x1002 | 0x1022 => "AMD",
        0x8086 => "Intel",
        0x5143 | 0x4d4f_4351 => "Qualcomm",
        0x13b5 => "Arm",
        0x1414 => "Microsoft",
        _ => return None,
    })
}

fn wide_to_string(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end]).trim().to_string()
}

/// Real adapters, the one with the most video memory first. Empty when DXGI
/// is unavailable.
fn read_adapters() -> Vec<GpuAdapter> {
    let Ok(factory) = (unsafe { CreateDXGIFactory1::<IDXGIFactory1>() }) else {
        return Vec::new();
    };
    let mut out: Vec<GpuAdapter> = Vec::new();
    for i in 0..MAX_ADAPTERS {
        // DXGI_ERROR_NOT_FOUND ends the list.
        let Ok(adapter) = (unsafe { factory.EnumAdapters1(i) }) else { break };
        let Ok(desc) = (unsafe { adapter.GetDesc1() }) else { continue };
        let software = desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 != 0;
        let name = wide_to_string(&desc.Description);
        if software || name.is_empty() || (desc.VendorId == VENDOR_MICROSOFT && desc.DedicatedVideoMemory == 0) {
            continue;
        }
        let luid = luid_key(desc.AdapterLuid.HighPart as u32, desc.AdapterLuid.LowPart);
        if out.iter().any(|a| a.luid == luid) {
            continue;
        }
        let driver_version = unsafe { adapter.CheckInterfaceSupport(&IDXGIDevice::IID) }
            .ok()
            .and_then(driver_version_text);
        out.push(GpuAdapter {
            luid,
            name,
            vendor: vendor_name(desc.VendorId).map(str::to_string),
            dedicated_memory: desc.DedicatedVideoMemory as u64,
            shared_memory: desc.SharedSystemMemory as u64,
            driver_version,
        });
    }
    // Stable: equal memory keeps DXGI's order (the primary adapter first).
    out.sort_by(|a, b| b.dedicated_memory.cmp(&a.dedicated_memory));
    out
}

/// Cached: adapters do not change while Orbit runs (a driver update restarts
/// the display stack; the old version shows until Orbit restarts).
pub fn adapters() -> Vec<GpuAdapter> {
    static CACHE: OnceLock<Vec<GpuAdapter>> = OnceLock::new();
    CACHE.get_or_init(read_adapters).clone()
}

/// Leaves out an adapter with no GPU counters when an adapter of the same
/// name and memory has them (or, while no counters are known, comes first).
/// Two identical cards that both have counters both stay.
pub fn visible(all: Vec<GpuAdapter>, with_counters: &[String]) -> Vec<GpuAdapter> {
    let counted = |a: &GpuAdapter| with_counters.iter().any(|l| *l == a.luid);
    let twin = |a: &GpuAdapter, b: &GpuAdapter| {
        a.luid != b.luid && a.name == b.name && a.dedicated_memory == b.dedicated_memory
    };
    let mut out: Vec<GpuAdapter> = Vec::new();
    for (i, a) in all.iter().enumerate() {
        if !counted(a) {
            let shadowed = all.iter().enumerate().any(|(j, b)| {
                twin(a, b) && (counted(b) || (j < i && with_counters.is_empty()))
            });
            if shadowed {
                continue;
            }
        }
        out.push(a.clone());
    }
    out
}

/// LUIDs that had GPU counters in the latest snapshot.
pub fn counter_luids(state: &crate::sampler::AppState) -> Vec<String> {
    let guard = state.latest.lock().unwrap_or_else(|e| e.into_inner());
    guard
        .as_ref()
        .map(|s| s.gpu.adapters.iter().map(|a| a.luid.clone()).collect())
        .unwrap_or_default()
}

#[tauri::command]
pub async fn get_gpu_info(state: State<'_, crate::sampler::AppState>) -> Result<Vec<GpuAdapter>, String> {
    let luids = counter_luids(&state);
    let all = tauri::async_runtime::spawn_blocking(adapters).await.unwrap_or_default();
    Ok(visible(all, &luids))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn luid_keys() {
        assert_eq!(luid_key(0, 0xD1A5), "0x00000000_0x0000d1a5");
        assert_eq!(luid_key(1, 0xffff_ffff), "0x00000001_0xffffffff");
    }

    #[test]
    fn driver_versions() {
        let v: i64 = (32 << 48) | (15 << 16) | 9621;
        assert_eq!(driver_version_text(v).as_deref(), Some("32.0.15.9621"));
        assert_eq!(driver_version_text(0), None);
        let high: i64 = ((0xffffu64 << 48) | 1) as i64;
        assert_eq!(driver_version_text(high).as_deref(), Some("65535.0.0.1"));
    }

    #[test]
    fn vendors() {
        assert_eq!(vendor_name(0x10de), Some("NVIDIA"));
        assert_eq!(vendor_name(0x1002), Some("AMD"));
        assert_eq!(vendor_name(0x8086), Some("Intel"));
        assert_eq!(vendor_name(0x1234), None);
    }

    fn adapter(luid: &str, name: &str, mem: u64) -> GpuAdapter {
        GpuAdapter {
            luid: luid.into(),
            name: name.into(),
            vendor: None,
            dedicated_memory: mem,
            shared_memory: 0,
            driver_version: None,
        }
    }

    #[test]
    fn drops_virtual_twins() {
        let all = vec![adapter("a", "RTX", 8), adapter("b", "RTX", 8), adapter("c", "UHD", 1)];
        let luids = |v: &[GpuAdapter]| v.iter().map(|a| a.luid.clone()).collect::<Vec<_>>();
        // The twin with counters wins, wherever it is.
        assert_eq!(luids(&visible(all.clone(), &["b".into(), "c".into()])), ["b", "c"]);
        assert_eq!(luids(&visible(all.clone(), &["a".into()])), ["a", "c"]);
        // No counters yet: the first of the twins stays.
        assert_eq!(luids(&visible(all.clone(), &[])), ["a", "c"]);
        // Two identical cards with counters: both stay.
        assert_eq!(luids(&visible(all, &["a".into(), "b".into()])), ["a", "b", "c"]);
    }

    /// Reads this PC's adapters: never panics, no software adapter, unique LUIDs.
    #[test]
    fn reads_real_adapters() {
        let list = read_adapters();
        for a in &list {
            assert!(!a.name.is_empty());
            assert!(!a.name.contains("Basic Render"), "{}", a.name);
            assert_eq!(list.iter().filter(|b| b.luid == a.luid).count(), 1);
        }
        println!("adapters: {list:#?}");
    }
}
