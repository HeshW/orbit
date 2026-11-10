// Orbit / mock/sensors: Snapshot.sensors for the browser preview (sensors.rs, docs/CONTRACT.md
// "v0.7"). The CPU reads like LibreHardwareMonitor on the mock's 4-core i7-7700K (package,
// four cores, power and clocks); the NVIDIA card has every NVML sensor; the Intel card has no
// library Orbit can read, so it is not in the list (as on a real PC). Temperatures follow the
// load, so a busy mock PC turns warm.
// URL flags: ?nocputemp (no tool running: cpu is null), ?hot (CPU and GPU past the hot
// thresholds), ?nogpu (no GPU sensors either).

import { ADAPTERS } from './gpu.js';

const params = typeof location !== 'undefined' ? new URLSearchParams(location.search) : null;
const NO_CPU = !!params && params.has('nocputemp');
const NO_GPU = !!params && params.has('nogpu');
const HOT = !!params && params.has('hot');
const CORES = 4; // physical cores, as get_static_info reports (api.js PHYSICAL)

const clamp = (v, lo, hi) => (v < lo ? lo : v > hi ? hi : v);
const r1 = (v) => Math.round(v * 10) / 10;
let cpuT = HOT ? 93 : 48;
let gpuT = HOT ? 89 : 44;

// Ease toward a target so the 60-second graph moves like a real sensor.
function ease(cur, target) {
  return cur + (target - cur) * 0.25 + (Math.random() - 0.5) * 0.6;
}

/** Snapshot.sensors for a mock Snapshot (cpu.usage, gpu.usage, cpu.freqMhz already set). */
export function sensorsFor(snap) {
  const load = snap.cpu ? snap.cpu.usage || 0 : 0;
  const gload = snap.gpu && typeof snap.gpu.usage === 'number' ? snap.gpu.usage : 0;
  cpuT = clamp(ease(cpuT, HOT ? 93 : 42 + load * 0.55), 30, 99);
  gpuT = clamp(ease(gpuT, HOT ? 89 : 40 + gload * 0.5), 30, 95);
  const freq = (snap.cpu && snap.cpu.freqMhz) || 4200;

  const cpu = NO_CPU ? null : {
    source: 'LibreHardwareMonitor',
    approximate: false,
    package: r1(cpuT),
    cores: Array.from({ length: CORES }, (_, i) => ({
      name: `Core ${i + 1}`,
      value: r1(clamp(cpuT - 4 + ((i * 7) % 6) + Math.random() * 1.5, 25, 105)),
    })),
    power: r1(18 + load * 0.9 + Math.random() * 2),
    clocks: Array.from({ length: CORES }, (_, i) => ({
      name: `Core ${i + 1}`,
      value: Math.round(freq - 120 + ((i * 53) % 160) + Math.random() * 40),
    })),
  };

  const gpus = NO_GPU ? [] : [{
    luid: ADAPTERS[0].luid,
    name: ADAPTERS[0].name,
    source: 'NVML',
    temp: Math.round(gpuT),
    memoryTemp: Math.round(gpuT + 6),
    slowdownTemp: 87,
    fanPct: Math.round(clamp(30 + gload * 0.6, 0, 100)),
    powerW: r1(9 + gload * 1.1),
    powerLimitW: 115,
    coreMhz: Math.round(gload > 5 ? 1980 + gload * 6 : 210),
    memoryMhz: gload > 5 ? 8001 : 405,
  }];

  return { cpu, gpus };
}
