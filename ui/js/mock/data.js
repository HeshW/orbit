// Orbit / mock/data: the v0.4 snapshot fields in the browser preview (data feature,
// docs/V04-CONTRACT.md). api.js calls decorateRow for every ProcessRow it builds and
// decorateSnapshot for every Snapshot, after the v0.2 fields are filled in.
//
// Shapes and rounding follow sampler.rs: disk names are PDH PhysicalDisk instances
// ("0 C: E:"), activePct has two decimals, byte rates are whole numbers, GPU engines come
// in the order 3D, Copy, Video Encode, Video Decode, then the rest by name.
// URL flags: ?nogpu (no GPU counters: gpuEngines is []), ?nobattery (a desktop PC:
// battery is null, as on most desktops).

const clamp = (v, lo, hi) => (v < lo ? lo : v > hi ? hi : v);
const round2 = (v) => Math.round(v * 100) / 100;

const params = typeof location !== 'undefined' ? new URLSearchParams(location.search) : null;
const NO_BATTERY = !!params && params.has('nobattery');

// Users whose processes run in the signed-in session (1); everything else is session 0.
const SESSION_1_USERS = /^(hesh|dwm-1|umfd-1)$/i;
// The signed-in session's own csrss.exe (its smss.exe has exited) and winlogon.exe run
// as SYSTEM but in session 1, as on a real PC.
const SESSION_1_CSRSS_PARENT = 4 * 131;

const battery = { percent: 84, charging: false };

/** Fill the v0.4 ProcessRow fields of a mock row. */
export function decorateRow(row) {
  const session1 = SESSION_1_USERS.test(row.user || '')
    || row.name === 'winlogon.exe'
    || (row.name === 'csrss.exe' && row.parentPid === SESSION_1_CSRSS_PARENT);
  row.sessionId = session1 ? 1 : 0;
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
    { name: '0 C: E:', activePct: round2(clamp(active * 0.85 + Math.random(), 0, 100)), readBps: Math.round(read * 0.82), writeBps: Math.round(write * 0.9) },
    { name: '1 D:', activePct: round2(clamp(active * 0.3 + Math.random() * 0.5, 0, 100)), readBps: Math.round(read * 0.18), writeBps: Math.round(write * 0.1) },
  ];

  // Busiest engine of each type that some process has opened, idle ones included.
  const gpu = snap.gpu && typeof snap.gpu.usage === 'number' ? snap.gpu.usage : null;
  snap.gpuEngines = gpu === null ? [] : [
    { name: '3D', usage: round2(clamp(gpu * 0.85, 0, 100)) },
    { name: 'Copy', usage: round2(clamp(gpu * 0.12 + Math.random() * 0.6, 0, 100)) },
    { name: 'Video Encode', usage: 0 },
    { name: 'Video Decode', usage: round2(clamp(2.5 + Math.random() * 1.5, 0, 100)) },
    { name: 'Security', usage: 0 },
  ];

  if (NO_BATTERY) {
    snap.battery = null;
  } else {
    battery.percent = clamp(battery.percent - Math.random() * 0.01, 5, 100);
    const pctLeft = Math.round(battery.percent);
    snap.battery = {
      percent: pctLeft,
      charging: battery.charging,
      acOnline: battery.charging,
      // Windows gives no estimate while plugged in.
      secondsLeft: battery.charging ? null : Math.round(pctLeft * 108),
    };
  }

  if (snap.network) snap.network.perApp = { available: false, reason: 'Needs administrator rights.' };
  return snap;
}

// No commands of its own: the snapshot comes from api.js.
export default {};
