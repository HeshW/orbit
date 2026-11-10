# Feature: GPU names (v0.6, 8a), Performance > GPU

The GPU pane names the graphics card instead of "Graphics", and lists every adapter with its
video memory, driver version and live load. Shapes and the command are in `docs/CONTRACT.md`
("v0.6").

Files: `src-tauri/src/gpu.rs`, `ui/js/mock/gpu.js`. Shared files touched: `lib.rs` (module +
`get_gpu_info`), `Cargo.toml` (`Win32_Graphics_Dxgi`), `model.rs` (`GpuStats.adapters`,
`GpuAdapterLoad`), `pdh.rs` (per-adapter load), `sampler.rs`, `history.rs` (test helper),
`views/performance.js`, `css/views/performance.css`, `mock/data.js`, `mock/index.js`,
`tests/ui-smoke.mjs`.

## What it shows

- Pane title: the first adapter's name (the one with the most video memory, so the discrete
  card on a laptop). "Graphics" until the list arrives or when Windows names none.
- "Adapters / N": one card per adapter: name, live usage (busiest engine of that adapter), a
  bar, "8 GB VRAM · Driver 32.0.15.9621" and "2.4 GB in use". An integrated GPU with no
  dedicated memory shows its shared memory instead ("16 GB shared").
- The GPU tile's tooltip lists the adapter names.

## Rules

- Names and memory: DXGI `CreateDXGIFactory1` + `EnumAdapters1` + `GetDesc1`. No admin, no
  WMI. Read once per run and cached (`gpu::adapters`).
- Left out: `DXGI_ADAPTER_FLAG_SOFTWARE` (Microsoft Basic Render Driver) and Microsoft-vendor
  adapters with no video memory (remote / virtual displays).
- Driver version: `IDXGIAdapter::CheckInterfaceSupport(IDXGIDevice)` returns the user-mode
  driver version; its four 16-bit parts are the version Device Manager shows. NVIDIA's own
  "596.21" form is not derived.
- Matching: every adapter carries its LUID as `0x<high>_0x<low>` (8 lowercase hex digits each,
  `gpu::luid_key`). `pdh.rs` turns the counter instance `luid_0x00000000_0x0000D1A5` into the
  same key (`adapter_luid`) and reports `Snapshot.gpu.adapters`: per LUID, the busiest engine
  and the adapter's dedicated memory in use (`\GPU Adapter Memory(*)\Dedicated Usage`, or the
  per-process sum when that counter is missing). Totals (`gpu.usage`, `gpu.dedicatedUsed`)
  are unchanged.
- Twins: on the dev PC DXGI lists the RTX 4060 twice (a second LUID added by a virtual
  display driver, SuperDisplay); only the first has GPU counters. `get_gpu_info` drops an
  adapter with no counters when another adapter with the same name and memory has them
  (`gpu::visible`, using the latest snapshot); before the first snapshot it keeps the first
  of the twins. Two identical real cards both have counters, so both stay.

## How to test

- `cargo test gpu::` (LUID format, driver version text, vendors, twin rule, and a read of
  this PC's adapters printed with `--nocapture`) and `cargo test pdh::` (`adapter_luid`).
- Browser preview: Performance > GPU shows two adapters (RTX 4070 Laptop GPU and UHD
  Graphics 770). `tests/ui-smoke.mjs` checks the names, "VRAM" and "Driver".
