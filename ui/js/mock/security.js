// Orbit / mock/security: signatures, the security scan, hashes, modules and environment
// for the browser preview (security feature, docs/V04-CONTRACT.md).

const PUBLISHERS = [
  [/^c:\\windows\\/i, 'Microsoft Windows'],
  [/\\microsoft vs code\\/i, 'Microsoft Corporation'],
  [/\\microsoft\\edge/i, 'Microsoft Corporation'],
  [/\\google\\chrome\\/i, 'Google LLC'],
  [/\\discord\\/i, 'Discord Inc.'],
  [/\\steam/i, 'Valve Corp.'],
  [/\\nvidia/i, 'NVIDIA Corporation'],
  [/\\obs-studio\\/i, 'OBS Project'],
  [/\\adobe\\/i, 'Adobe Inc.'],
  [/\\orbit\\/i, null], // Orbit itself is not code-signed yet
];

function signatureOf(path) {
  const p = String(path || '');
  if (!p) return { status: 'unknown', publisher: null };
  if (/\\(temp|downloads)\\/i.test(p)) return { status: 'unsigned', publisher: null };
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

export default {
  get_signatures({ paths }) {
    const out = {};
    for (const p of Array.isArray(paths) ? paths : []) out[p] = signatureOf(p);
    return out;
  },

  async security_scan(_args, ctx) {
    await ctx.delay(900); // a real scan checks every signature
    const rows = rowsOf(ctx);
    const findings = [];
    const discord = rows.find((r) => r.name === 'Discord.exe' && r.exe);
    if (discord) {
      findings.push({
        pid: discord.pid, name: discord.name, exe: discord.exe, severity: 'info',
        reasons: ['Runs from your user profile (AppData), not Program Files.'],
        signatureStatus: 'signed', publisher: 'Discord Inc.',
      });
    }
    const own = rows.find((r) => r.name === 'orbit.exe' && r.exe);
    if (own) {
      findings.push({
        pid: own.pid, name: own.name, exe: own.exe, severity: 'medium',
        reasons: ['Not signed.', 'Runs from your user profile (AppData), not Program Files.'],
        signatureStatus: 'unsigned', publisher: null,
      });
    }
    findings.unshift({
      pid: 9172, name: 'svch0st.exe', exe: 'C:\\Users\\Hesh\\AppData\\Local\\Temp\\svch0st.exe', severity: 'high',
      reasons: ['Name looks like a Windows process (svchost.exe) but it is not.', 'Runs from a temporary folder.', 'Not signed.'],
      signatureStatus: 'unsigned', publisher: null,
    });
    return findings;
  },

  async file_sha256({ path }, ctx) {
    if (!path) throw 'Pick a file first.';
    await ctx.delay(300);
    return fakeSha(path);
  },

  get_process_modules({ pid }, ctx) {
    const row = rowsOf(ctx).find((r) => r.pid === Number(pid));
    if (!row) throw 'That process has exited.';
    if (!row.exe) throw 'Access denied. Try Restart as administrator.';
    const sys = 'C:\\Windows\\System32';
    return [
      { path: row.exe, size: 2_457_600 },
      { path: `${sys}\\ntdll.dll`, size: 2_011_136 },
      { path: `${sys}\\kernel32.dll`, size: 774_144 },
      { path: `${sys}\\KERNELBASE.dll`, size: 2_928_640 },
      { path: `${sys}\\user32.dll`, size: 1_626_112 },
      { path: `${sys}\\gdi32.dll`, size: 151_552 },
      { path: `${sys}\\advapi32.dll`, size: 667_648 },
      { path: `${sys}\\combase.dll`, size: 3_489_792 },
      { path: `${sys}\\ws2_32.dll`, size: 413_696 },
    ];
  },

  get_process_environment({ pid }, ctx) {
    const row = rowsOf(ctx).find((r) => r.pid === Number(pid));
    if (!row) throw 'That process has exited.';
    if (row.user !== 'Hesh' && !ctx.isAdmin()) throw 'Access denied. Try Restart as administrator.';
    return [
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
