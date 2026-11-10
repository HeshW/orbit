// Orbit / mock: Microsoft Defender status and scans (src-tauri/src/defender.rs) for the
// browser preview. Same shapes as Rust. Scans are short and fake: a quick scan takes about
// 3 s, a full scan 6 s, a custom scan 1.5 s; each sends orbit://defender-progress about
// once a second and can be cancelled. A custom scan of a file named like a crack, keygen or
// one of the mock's malware-like processes "finds" threats; everything else is clean.
//
// ?otherav in the URL: Norton 360 is the active antivirus and Defender is passive, so the
// panel names Norton and hides the scan buttons.

const EVENT = 'orbit://defender-progress';
const OTHER_AV = typeof location !== 'undefined' && new URLSearchParams(location.search).has('otherav');
const SECS = { quick: 3, full: 6, custom: 1.5 };
const THREAT_NAME = /crack|keygen|svch0st|chromeupdate|wacatac/i;
const now = () => Math.floor(Date.now() / 1000);

const state = {
  current: null,
  last: null,
  nextId: 1,
  cancel: false,
  quickAt: now() - 26 * 3600,
  fullAt: null,
};

function status(ctx) {
  const products = OTHER_AV
    ? [
      { name: 'Windows Defender', defender: true, state: 'off', upToDate: true },
      { name: 'Norton 360', defender: false, state: 'on', upToDate: true },
    ]
    : [{ name: 'Windows Defender', defender: true, state: 'on', upToDate: true }];
  return {
    readAt: now(),
    isAdmin: ctx.isAdmin(),
    available: true,
    active: !OTHER_AV,
    runningMode: OTHER_AV ? 'Passive Mode' : 'Normal',
    realTime: !OTHER_AV,
    signatureVersion: '1.463.223.0',
    signatureUpdated: now() - 5 * 3600,
    signatureAgeDays: 0,
    signaturesOutOfDate: false,
    quickScanAt: state.quickAt,
    fullScanAt: state.fullAt,
    engineVersion: '1.1.26100.4',
    productVersion: '4.18.26100.4',
    tamperProtected: true,
    products,
    other: OTHER_AV ? products[1] : null,
    canScan: !OTHER_AV,
    scan: state.current || state.last,
    error: null,
  };
}

function validTarget(path) {
  const p = String(path || '').trim();
  if (!p) throw 'Pick a file or folder to scan.';
  if (!/^[a-z]:\\/i.test(p) || /["*?<>|]/.test(p) || p.slice(2).includes(':')) throw 'Orbit can only scan a file or folder on this PC.';
  return p.length > 3 ? p.replace(/\\+$/, '') : p;
}

export default {
  defender_status(_args, ctx) {
    return status(ctx);
  },

  async defender_scan({ kind, path }, ctx) {
    if (!SECS[kind]) throw 'Unknown scan type.';
    const target = kind === 'custom' ? validTarget(path) : null;
    if (OTHER_AV) throw 'Microsoft Defender is not running, so it can’t scan.';
    if (state.current) throw 'A Defender scan is already running. Wait for it to finish or cancel it.';
    const scan = {
      id: state.nextId++, kind, target, state: 'running', startedAt: now(), elapsedSecs: 0,
      threats: [], message: null, needsAdmin: false, exitCode: null,
    };
    state.current = scan;
    state.cancel = false;
    const started = Date.now();
    ctx.emit(EVENT, { ...scan });
    while (Date.now() - started < SECS[kind] * 1000 && !state.cancel) {
      await ctx.delay(250);
      const secs = Math.floor((Date.now() - started) / 1000);
      if (secs !== scan.elapsedSecs) {
        scan.elapsedSecs = secs;
        ctx.emit(EVENT, { ...scan });
      }
    }
    scan.elapsedSecs = Math.floor((Date.now() - started) / 1000);
    if (state.cancel) {
      Object.assign(scan, { state: 'cancelled', message: 'Scan cancelled.' });
    } else if (target && THREAT_NAME.test(target)) {
      Object.assign(scan, { state: 'threats', threats: ['Trojan:Win32/Wacatac.B!ml', 'PUA:Win32/Presenoker'], exitCode: 2 });
    } else {
      Object.assign(scan, { state: 'clean', exitCode: 0 });
      if (kind === 'quick') state.quickAt = now();
      if (kind === 'full') state.fullAt = now();
    }
    state.current = null;
    state.last = { ...scan };
    ctx.emit(EVENT, { ...scan });
    return { ...scan };
  },

  defender_cancel() {
    if (state.current) state.cancel = true;
    return null;
  },

  defender_pick_target({ folder }) {
    return folder ? 'C:\\Users\\you\\Documents' : 'C:\\Users\\you\\Downloads\\game-keygen.exe';
  },

  open_windows_security() {
    return null;
  },
};
