// Orbit / mock/security: signatures, the security scan, hashes, modules and environment
// for the browser preview (security feature, docs/V04-CONTRACT.md).
//
// security_scan mirrors the Rust rules for the mock's own processes (one finding per
// program file, the oldest process running it): unsigned = medium, a user profile
// (AppData) = info, signed Windows files in System32 = never. It adds two high findings
// that the real scan would raise for malware-like files.

const PUBLISHERS = [
  [/^c:\\windows\\system32\\driverstore\\.*realtek/i, 'Realtek Semiconductor Corp.'],
  [/^c:\\windows\\system32\\driverstore\\.*nv_/i, 'NVIDIA Corporation'],
  [/^c:\\windows\\/i, 'Microsoft Windows'],
  [/\\windowsapps\\microsoft\./i, 'Microsoft Corporation'],
  [/\\microsoft\\onedrive\\/i, 'Microsoft Corporation'],
  [/\\microsoft vs code\\/i, 'Microsoft Corporation'],
  [/\\microsoft\\edge/i, 'Microsoft Corporation'],
  [/\\google\\chrome\\application\\/i, 'Google LLC'],
  [/\\discord\\/i, 'Discord Inc.'],
  [/\\spotify\\/i, 'Spotify AB'],
  [/\\notion\\/i, 'Notion Labs, Incorporated'],
  [/\\steam/i, 'Valve Corp.'],
  [/\\nvidia/i, 'NVIDIA Corporation'],
  [/\\obs-studio\\/i, 'OBS Project'],
  [/\\adobe/i, 'Adobe Inc.'],
  [/\\nodejs\\/i, 'OpenJS Foundation'],
  [/\\orbit\\/i, null], // Orbit itself is not code-signed yet
];

function signatureOf(path) {
  const p = String(path || '');
  if (!p) return { status: 'unknown', publisher: null };
  if (/\\(temp|downloads)\\|\\user data\\/i.test(p)) return { status: 'unsigned', publisher: null };
  for (const [re, publisher] of PUBLISHERS) {
    if (re.test(p)) return publisher ? { status: 'signed', publisher } : { status: 'unsigned', publisher: null };
  }
  return { status: 'unknown', publisher: null };
}

// A stable fake SHA-256 (64 hex chars) for a path.
function fakeSha(path) {
  let h1 = 0x811c9dc5;
  let out = '';
  for (let round = 0; round < 8; round++) {
    for (const ch of `${round}:${path}`) h1 = Math.imul(h1 ^ ch.charCodeAt(0), 16777619) >>> 0;
    out += h1.toString(16).padStart(8, '0');
  }
  return out;
}

function rowsOf(ctx) {
  const snap = ctx.snapshot();
  return snap && Array.isArray(snap.processes) ? snap.processes : [];
}

const SEV_ORDER = { high: 0, medium: 1, info: 2 };

// Malware-like files the mock PC "runs"; their pids are not in the mock process list,
// so the view shows them as exited.
const FAKE_HIGH = [
  {
    pid: 11408, name: 'chromeupdate.exe', severity: 'high',
    exe: 'C:\\Users\\Hesh\\AppData\\Local\\Google\\Chrome\\User Data\\Default\\chromeupdate.exe',
    reasons: ['The file is hidden.', 'Not signed.', 'Runs from a browser profile folder.'],
    signatureStatus: 'unsigned', publisher: null,
  },
  {
    pid: 9172, name: 'svch0st.exe', severity: 'high',
    exe: 'C:\\Users\\Hesh\\AppData\\Local\\Temp\\svch0st.exe',
    reasons: ['Name looks like a Windows process (svchost.exe) but it is not.', 'Not signed.', 'Runs from a temporary folder.'],
    signatureStatus: 'unsigned', publisher: null,
  },
];

function evaluate(row) {
  const sig = signatureOf(row.exe);
  const p = row.exe.toLowerCase();
  if (sig.status === 'signed' && /^microsoft/i.test(sig.publisher || '') && /^c:\\windows\\(system32|syswow64)\\/.test(p)) return null;
  const reasons = [];
  let severity = null;
  if (sig.status === 'unsigned' || sig.status === 'invalid') {
    reasons.push(sig.status === 'invalid' ? 'The signature is not valid.' : 'Not signed.');
    severity = 'medium';
  }
  if (p.includes('\\appdata\\')) {
    reasons.push('Runs from a user profile (AppData), not Program Files.');
    severity = severity || 'info';
  }
  if (!severity) return null;
  return {
    pid: row.pid, name: row.name, exe: row.exe, severity, reasons,
    signatureStatus: sig.status, publisher: sig.publisher,
  };
}

export default {
  get_signatures({ paths }) {
    const out = {};
    for (const p of Array.isArray(paths) ? paths : []) out[p] = signatureOf(p);
    return out;
  },

  async security_scan(_args, ctx) {
    await ctx.delay(900); // a real scan checks every signature
    const oldest = new Map();
    for (const r of rowsOf(ctx)) {
      if (!r.exe || r.pid === 0 || r.pid === 4) continue;
      const key = r.exe.toLowerCase();
      const cur = oldest.get(key);
      if (!cur || r.startTime < cur.startTime || (r.startTime === cur.startTime && r.pid < cur.pid)) oldest.set(key, r);
    }
    const findings = [...FAKE_HIGH.map((f) => ({ ...f, reasons: [...f.reasons] }))];
    for (const r of oldest.values()) {
      const f = evaluate(r);
      if (f) findings.push(f);
    }
    findings.sort((a, b) => (SEV_ORDER[a.severity] - SEV_ORDER[b.severity])
      || a.name.toLowerCase().localeCompare(b.name.toLowerCase()) || a.pid - b.pid);
    return findings;
  },

  async file_sha256({ path }, ctx) {
    if (!path) throw 'No file path is known for that process.';
    await ctx.delay(300);
    return fakeSha(path);
  },

  get_process_modules({ pid }, ctx) {
    const row = rowsOf(ctx).find((r) => r.pid === Number(pid));
    if (!row) throw 'That process has exited.';
    if (!row.exe) throw 'Access denied. Try Restart as administrator.';
    if (row.user !== 'Hesh' && !ctx.isAdmin()) throw 'Access denied. Try Restart as administrator.';
    const sys = 'C:\\Windows\\System32';
    return [
      { path: row.exe, size: 2_457_600 },
      { path: `${sys}\\ntdll.dll`, size: 2_011_136 },
      { path: `${sys}\\kernel32.dll`, size: 774_144 },
      { path: `${sys}\\KERNELBASE.dll`, size: 2_928_640 },
      { path: `${sys}\\user32.dll`, size: 1_626_112 },
      { path: `${sys}\\win32u.dll`, size: 151_552 },
      { path: `${sys}\\gdi32.dll`, size: 151_552 },
      { path: `${sys}\\advapi32.dll`, size: 667_648 },
      { path: `${sys}\\msvcrt.dll`, size: 636_792 },
      { path: `${sys}\\combase.dll`, size: 3_489_792 },
      { path: `${sys}\\ws2_32.dll`, size: 413_696 },
      { path: `${sys}\\crypt32.dll`, size: 1_548_288 },
    ];
  },

  get_process_environment({ pid }, ctx) {
    const row = rowsOf(ctx).find((r) => r.pid === Number(pid));
    if (!row) throw 'That process has exited.';
    if (row.user !== 'Hesh' && !ctx.isAdmin()) throw 'Access denied. Try Restart as administrator.';
    return [
      { name: '=C:', value: 'C:\\Users\\Hesh' },
      { name: 'ALLUSERSPROFILE', value: 'C:\\ProgramData' },
      { name: 'APPDATA', value: 'C:\\Users\\Hesh\\AppData\\Roaming' },
      { name: 'COMPUTERNAME', value: 'LEGACY-PC' },
      { name: 'LOCALAPPDATA', value: 'C:\\Users\\Hesh\\AppData\\Local' },
      { name: 'NUMBER_OF_PROCESSORS', value: '8' },
      { name: 'OS', value: 'Windows_NT' },
      { name: 'Path', value: 'C:\\Windows\\system32;C:\\Windows;C:\\Windows\\System32\\Wbem;C:\\Program Files\\Git\\cmd;C:\\Users\\Hesh\\.cargo\\bin' },
      { name: 'TEMP', value: 'C:\\Users\\Hesh\\AppData\\Local\\Temp' },
      { name: 'USERNAME', value: 'Hesh' },
      { name: 'USERPROFILE', value: 'C:\\Users\\Hesh' },
      { name: 'windir', value: 'C:\\Windows' },
    ];
  },
};
