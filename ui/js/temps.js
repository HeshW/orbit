// Orbit / temps: temperature thresholds, levels and text, in one place (Performance
// tiles and Sensors blocks, 0.7). Readings arrive in °C (Snapshot.sensors, sensors.rs);
// settings.tempUnit 'f' only changes how they are written.

/** °C where a reading turns warm, then hot. */
export const TEMP_LIMITS = Object.freeze({
  cpu: Object.freeze({ warm: 80, hot: 90 }),
  gpu: Object.freeze({ warm: 80, hot: 87 }),
});

/** Where each source's numbers come from, as the Sensors blocks name it. */
export const SOURCE_NAMES = Object.freeze({
  NVML: "NVIDIA's driver (NVML)",
  LibreHardwareMonitor: 'LibreHardwareMonitor',
  OpenHardwareMonitor: 'OpenHardwareMonitor',
  HWiNFO: 'HWiNFO',
  ACPI: 'ACPI thermal zone',
});

const valid = (c) => typeof c === 'number' && Number.isFinite(c);

/** 'normal' | 'warm' | 'hot' for a °C reading of kind 'cpu' or 'gpu'; null without one. */
export function tempLevel(kind, c) {
  if (!valid(c)) return null;
  const lim = TEMP_LIMITS[kind] || TEMP_LIMITS.cpu;
  if (c >= lim.hot) return 'hot';
  if (c >= lim.warm) return 'warm';
  return 'normal';
}

/** "62°C" or "144°F" (unit 'c' | 'f'); "–" without a reading. */
export function tempText(c, unit = 'c', dp = 0) {
  if (!valid(c)) return '–';
  const v = unit === 'f' ? c * 9 / 5 + 32 : c;
  return `${v.toFixed(dp)}°${unit === 'f' ? 'F' : 'C'}`;
}

/** Set (or clear) the temp--normal / --warm / --hot class on an element; cheap per tick. */
export function setLevel(el, level) {
  if (!el || el.__lvl === level) return;
  el.classList.remove('temp--normal', 'temp--warm', 'temp--hot');
  if (level) el.classList.add(`temp--${level}`);
  el.__lvl = level;
}
