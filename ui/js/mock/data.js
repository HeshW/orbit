// Orbit / mock/data: the v0.4 snapshot fields in the browser preview (data feature,
// docs/V04-CONTRACT.md). api.js calls decorateRow for every ProcessRow it builds and
// decorateSnapshot for every Snapshot, after the v0.2 fields are filled in.

const clamp = (v, lo, hi) => (v < lo ? lo : v > hi ? hi : v);
const round1 = (v) => Math.round(v * 10) / 10;

// Users whose processes run in the signed-in session (1); everything else is session 0.
const SESSION_1_USERS = /^(hesh|dwm-1|umfd-1)$/i;

const battery = { percent: 84, charging: false };

/** Fill the v0.4 ProcessRow fields of a mock row. */
export function decorateRow(row) {
  row.sessionId = SESSION_1_USERS.test(row.user || '') ? 1 : 0;
  // Per-app network needs administrator rights (ETW): null in the unelevated preview.
  row.netRxBps = null;
  row.netTxBps = null;
  return row;
}

/** Fill the v0.4 Snapshot fields: per-disk activity, GPU engines, battery, per-app status. */
export function decorateSnapshot(snap) {
  const disk = snap.disk || {};
  const read = disk.readBps || 0;
  const write = disk.writeBps || 0;
  const active = disk.activePct || 0;
  // Two physical disks: the SSD holding C: and E: does most of the work.
  snap.disks = [
    { name: '0 C: E:', activePct: round1(clamp(active * 0.85 + Math.random(), 0, 100)), readBps: Math.round(read * 0.82), writeBps: Math.round(write * 0.9) },
    { name: '1 D:', activePct: round1(clamp(active * 0.3 + Math.random() * 0.5, 0, 100)), readBps: Math.round(read * 0.18), writeBps: Math.round(write * 0.1) },
  ];

  const gpu = snap.gpu && typeof snap.gpu.usage === 'number' ? snap.gpu.usage : null;
  snap.gpuEngines = gpu === null ? [] : [
    { name: '3D', usage: round1(clamp(gpu * 0.85, 0, 100)) },
    { name: 'Copy', usage: round1(clamp(gpu * 0.12 + Math.random() * 0.6, 0, 100)) },
    { name: 'Video Decode', usage: round1(clamp(2.5 + Math.random() * 1.5, 0, 100)) },
  ];

  battery.percent = clamp(battery.percent - Math.random() * 0.01, 5, 100);
  const pctLeft = Math.round(battery.percent);
  snap.battery = {
    percent: pctLeft,
    charging: battery.charging,
    acOnline: battery.charging,
    secondsLeft: battery.charging ? null : Math.round(pctLeft * 108),
  };

  if (snap.network) snap.network.perApp = { available: false, reason: 'Needs administrator rights.' };
  return snap;
}

// No commands of its own: the snapshot comes from api.js.
export default {};
