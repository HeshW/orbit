// Orbit / mock/history: App history for the browser preview (history feature,
// docs/V04-CONTRACT.md). 25 apps over the last 30 days, built once from a fixed seed so
// every reload shows the same numbers. Today is partial (scaled by the hour).
//
// Network bytes exist only for days 9 to 11 ago (as if Orbit ran as administrator then),
// plus today once the mock has "restarted as admin", like the real recorder.
// URL flags: ?nohistory (nothing recorded yet), ?historyfail (get_history throws).

const MB = 1024 ** 2;
const DAY = 86400;
const SPAN = 30;
const NET_DAYS = new Set([9, 10, 11]);

// [name, exe, pattern, cpu s/day, avg MB, max MB, read MB/day, write MB/day,
//  rx MB/day, tx MB/day, active h/day, running h/day, first seen days ago]
// pattern: daily, weekday, weekend, or a number = chance per day.
const APPS = [
  ['chrome.exe', 'C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe', 'daily', 2900, 1450, 3900, 820, 410, 2100, 140, 7.5, 12, 30],
  ['Adobe Premiere Pro.exe', 'C:\\Program Files\\Adobe\\Adobe Premiere Pro 2025\\Adobe Premiere Pro.exe', 'weekday', 5400, 2300, 6100, 9200, 4100, 15, 4, 3.2, 4.5, 21],
  ['Code.exe', 'C:\\Users\\Hesh\\AppData\\Local\\Programs\\Microsoft VS Code\\Code.exe', 'weekday', 1300, 820, 1600, 260, 190, 85, 22, 6.8, 9, 30],
  ['Discord.exe', 'C:\\Users\\Hesh\\AppData\\Local\\Discord\\app-1.0.9164\\Discord.exe', 'daily', 950, 540, 880, 120, 60, 640, 95, 2.1, 14, 30],
  ['obs64.exe', 'C:\\Program Files\\obs-studio\\bin\\64bit\\obs64.exe', 0.35, 3100, 410, 520, 40, 5200, 12, 2400, 1.9, 2.1, 30],
  ['steam.exe', 'C:\\Program Files (x86)\\Steam\\steam.exe', 'daily', 400, 180, 260, 1800, 2300, 4300, 60, 0.6, 16, 30],
  ['explorer.exe', 'C:\\Windows\\explorer.exe', 'daily', 380, 140, 210, 300, 45, 1, 0.5, 11, 16, 30],
  ['MsMpEng.exe', null, 'daily', 1700, 210, 330, 4100, 220, 30, 8, 2.4, 16, 30],
  ['SearchIndexer.exe', 'C:\\Windows\\System32\\SearchIndexer.exe', 'daily', 640, 48, 96, 1200, 380, 0, 0, 1.1, 16, 30],
  ['WindowsTerminal.exe', 'C:\\Program Files\\WindowsApps\\Microsoft.WindowsTerminal_1.21.2361.0_x64__8wekyb3d8bbwe\\WindowsTerminal.exe', 'weekday', 120, 88, 140, 20, 6, 0, 0, 4.4, 6, 18],
  ['orbit.exe', 'C:\\Users\\Hesh\\AppData\\Local\\Orbit\\orbit.exe', 'daily', 210, 92, 120, 15, 30, 0.4, 0.1, 10.5, 12, 6],
  ['Spotify.exe', 'C:\\Users\\Hesh\\AppData\\Roaming\\Spotify\\Spotify.exe', 'daily', 520, 260, 340, 140, 380, 920, 18, 5.6, 7, 25],
  ['Figma.exe', 'C:\\Users\\Hesh\\AppData\\Local\\Figma\\app-125.1.5\\Figma.exe', 'weekday', 1500, 960, 1900, 180, 90, 420, 60, 4.2, 5, 30],
  ['Photoshop.exe', 'C:\\Program Files\\Adobe\\Adobe Photoshop 2025\\Photoshop.exe', 0.4, 2200, 2100, 4800, 2600, 1700, 8, 2, 2.6, 3, 30],
  ['node.exe', 'C:\\Program Files\\nodejs\\node.exe', 'weekday', 1900, 310, 1200, 640, 520, 150, 90, 2.9, 8, 30],
  ['rustc.exe', 'C:\\Users\\Hesh\\.rustup\\toolchains\\stable-x86_64-pc-windows-msvc\\bin\\rustc.exe', 0.5, 4200, 780, 2400, 3100, 2900, 0, 0, 0.7, 0.8, 30],
  ['blender.exe', 'C:\\Program Files\\Blender Foundation\\Blender 4.2\\blender.exe', 0.2, 9800, 3200, 7400, 1500, 900, 2, 0.5, 2.2, 2.5, 30],
  ['OneDrive.exe', 'C:\\Users\\Hesh\\AppData\\Local\\Microsoft\\OneDrive\\OneDrive.exe', 'daily', 260, 120, 190, 900, 650, 380, 520, 0.3, 16, 30],
  ['ms-teams.exe', 'C:\\Program Files\\WindowsApps\\MSTeams_24295.605.3225.8804_x64__8wekyb3d8bbwe\\ms-teams.exe', 'weekday', 860, 610, 980, 90, 70, 700, 380, 3.5, 8, 30],
  ['dwm.exe', 'C:\\Windows\\System32\\dwm.exe', 'daily', 1100, 190, 420, 5, 2, 0, 0, 16, 16, 30],
  ['svchost.exe', 'C:\\Windows\\System32\\svchost.exe', 'daily', 1400, 920, 1300, 2200, 1400, 260, 120, 3.8, 16, 30],
  ['msedgewebview2.exe', 'C:\\Program Files (x86)\\Microsoft\\EdgeWebView\\Application\\130.0.2849.80\\msedgewebview2.exe', 'daily', 690, 640, 1100, 310, 260, 120, 30, 1.7, 15, 30],
  ['Notion.exe', 'C:\\Users\\Hesh\\AppData\\Local\\Programs\\Notion\\Notion.exe', 'weekday', 340, 480, 720, 60, 40, 110, 25, 1.8, 6, 30],
  ['cs2.exe', 'C:\\Program Files (x86)\\Steam\\steamapps\\common\\Counter-Strike Global Offensive\\game\\bin\\win64\\cs2.exe', 'weekend', 7600, 3900, 5200, 2100, 300, 640, 210, 3.1, 3.2, 30],
  ['Audacity.exe', 'C:\\Program Files\\Audacity\\Audacity.exe', 0.15, 480, 220, 610, 700, 1300, 0, 0, 1.2, 1.4, 30],
];

// Small seeded PRNG (mulberry32), so the preview is stable between reloads.
function rng(seed) {
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6d2b79f5) >>> 0;
    let t = a;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

function runsOn(pattern, weekday, rand) {
  if (pattern === 'daily') return true;
  if (pattern === 'weekday') return weekday >= 1 && weekday <= 5 ? rand() < 0.95 : rand() < 0.2;
  if (pattern === 'weekend') return weekday === 0 || weekday === 6 ? rand() < 0.9 : rand() < 0.12;
  return rand() < pattern;
}

/** days[d] = per-app records for the day d days ago (0 = today), built once. */
function buildDays() {
  const rand = rng(0x0b17);
  const now = new Date();
  const midnight = new Date(now.getFullYear(), now.getMonth(), now.getDate()).getTime() / 1000;
  const sinceMidnight = Math.max(0, Date.now() / 1000 - midnight);
  // Today so far, as a share of a full PC day (16 hours awake).
  const todayShare = Math.min(1, Math.max(0.05, sinceMidnight / (16 * 3600)));
  const days = [];
  for (let d = 0; d < SPAN; d++) {
    const dayStart = midnight - d * DAY;
    const weekday = new Date(dayStart * 1000).getDay();
    const list = [];
    APPS.forEach((app, i) => {
      const [, , pattern, cpu, avg, max, rd, wr, rx, tx, active, running, first] = app;
      const runs = runsOn(pattern, weekday, rand);
      const f = (0.55 + rand() * 0.9) * (d === 0 ? todayShare : 1);
      const memJitter = 0.8 + rand() * 0.4;
      const startHour = 7 + rand() * 3;
      if (!runs || d >= first) return;
      const runSecs = Math.min(running * 3600 * f, 23 * 3600);
      const start = dayStart + startHour * 3600;
      const end = d === 0 ? Math.min(Date.now() / 1000 - 30, start + runSecs) : start + runSecs;
      list.push({
        i,
        cpuSeconds: cpu * f,
        memByteSeconds: avg * memJitter * MB * runSecs,
        runSeconds: runSecs,
        maxMemory: max * memJitter * MB,
        diskReadBytes: rd * MB * f,
        diskWriteBytes: wr * MB * f,
        netRxBytes: NET_DAYS.has(d) ? rx * MB * f : 0,
        netTxBytes: NET_DAYS.has(d) ? tx * MB * f : 0,
        todayNetRx: rx * MB * f,
        todayNetTx: tx * MB * f,
        activeSeconds: Math.min(active * 3600 * f, runSecs),
        firstSeen: Math.floor(start),
        lastSeen: Math.floor(Math.max(start, end)),
      });
    });
    days.push(list);
  }
  // Apps open right now were seen a moment ago.
  const nowSecs = Math.floor(Date.now() / 1000);
  for (const rec of days[0]) {
    const pattern = APPS[rec.i][2];
    if (pattern === 'daily' || APPS[rec.i][0] === 'Adobe Premiere Pro.exe') rec.lastSeen = nowSecs - (rec.i % 3);
  }
  return days;
}

let days = null;
let cleared = false;

function flag(name) {
  try {
    return new URLSearchParams(location.search).has(name);
  } catch {
    return false;
  }
}

export default {
  async get_history({ days: want } = {}, ctx) {
    await ctx.delay(140);
    if (flag('historyfail')) throw 'Could not read app history.';
    if (cleared || flag('nohistory')) return [];
    if (!days) days = buildDays();
    const n = Math.min(90, Math.max(1, Math.round(Number(want) || 30)));
    const admin = ctx.isAdmin();
    const sums = new Map();
    for (let d = 0; d < Math.min(n, SPAN); d++) {
      for (const r of days[d]) {
        let s = sums.get(r.i);
        if (!s) {
          s = { cpu: 0, mbs: 0, run: 0, max: 0, rd: 0, wr: 0, rx: 0, tx: 0, active: 0, first: 0, last: 0 };
          sums.set(r.i, s);
        }
        s.cpu += r.cpuSeconds;
        s.mbs += r.memByteSeconds;
        s.run += r.runSeconds;
        s.max = Math.max(s.max, r.maxMemory);
        s.rd += r.diskReadBytes;
        s.wr += r.diskWriteBytes;
        s.rx += d === 0 && admin ? r.todayNetRx : r.netRxBytes;
        s.tx += d === 0 && admin ? r.todayNetTx : r.netTxBytes;
        s.active += r.activeSeconds;
        s.first = s.first ? Math.min(s.first, r.firstSeen) : r.firstSeen;
        s.last = Math.max(s.last, r.lastSeen);
      }
    }
    const rows = [];
    for (const [i, s] of sums) {
      const [name, exe] = APPS[i];
      rows.push({
        key: (exe || name).toLowerCase(),
        name,
        exe,
        cpuSeconds: Math.round(s.cpu * 1000) / 1000,
        maxMemory: Math.round(s.max),
        avgMemory: Math.round(s.run > 0 ? Math.min(s.mbs / s.run, s.max) : s.max),
        diskReadBytes: Math.round(s.rd),
        diskWriteBytes: Math.round(s.wr),
        netRxBytes: Math.round(s.rx),
        netTxBytes: Math.round(s.tx),
        activeSeconds: Math.round(s.active),
        firstSeen: s.first,
        lastSeen: s.last,
      });
    }
    rows.sort((a, b) => b.cpuSeconds - a.cpuSeconds || b.lastSeen - a.lastSeen);
    return rows;
  },

  async clear_history(_args, ctx) {
    await ctx.delay(80);
    cleared = true;
    return null;
  },

  // The preview records nothing, so the switch only needs an answer.
  set_history_enabled() {
    return null;
  },
};
