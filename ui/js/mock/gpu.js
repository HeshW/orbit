// Orbit / mock/gpu: graphics adapters for the browser preview (gpu.rs, docs/CONTRACT.md
// "v0.6"). A laptop-style pair: the discrete card first (most video memory), then the
// integrated one, as DXGI lists them after Orbit sorts. LUIDs use gpu::luid_key's form and
// match the Snapshot's gpu.adapters (mock/data.js decorateSnapshot).
// URL flag: ?nogpu (no GPU at all: an empty list, like a PC where DXGI fails).

const GB = 1024 ** 3;
const NO_GPU = typeof location !== 'undefined' && new URLSearchParams(location.search).has('nogpu');

export const ADAPTERS = [
  {
    luid: '0x00000000_0x0000d1a5',
    name: 'NVIDIA GeForce RTX 4070 Laptop GPU',
    vendor: 'NVIDIA',
    dedicatedMemory: 8 * GB,
    sharedMemory: 15.9 * GB,
    driverVersion: '32.0.15.6094',
  },
  {
    luid: '0x00000000_0x0000c3e2',
    name: 'Intel(R) UHD Graphics 770',
    vendor: 'Intel',
    dedicatedMemory: 128 * 1024 ** 2,
    sharedMemory: 15.9 * GB,
    driverVersion: '31.0.101.5186',
  },
];

export default {
  get_gpu_info() {
    return NO_GPU ? [] : ADAPTERS.map((a) => ({ ...a, sharedMemory: Math.round(a.sharedMemory) }));
  },
};
