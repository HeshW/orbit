// Orbit / mock/history: App history totals for the browser preview (history feature,
// docs/V04-CONTRACT.md). Totals scale with the number of days asked for.

const MB = 1024 ** 2;
const DAY = 86400;

// [name, exe, cpu seconds/day, avg memory MB, max memory MB, read MB/day, write MB/day,
//  rx MB/day, tx MB/day, active hours/day, first seen days ago]
const APPS = [
  ['chrome.exe', 'C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe', 2900, 1450, 3900, 820, 410, 2100, 140, 7.5, 30],
  ['Adobe Premiere Pro.exe', 'C:\\Program Files\\Adobe\\Adobe Premiere Pro 2025\\Adobe Premiere Pro.exe', 5400, 2300, 6100, 9200, 4100, 15, 4, 3.2, 21],
  ['Code.exe', 'C:\\Users\\Hesh\\AppData\\Local\\Programs\\Microsoft VS Code\\Code.exe', 1300, 820, 1600, 260, 190, 85, 22, 6.8, 30],
  ['Discord.exe', 'C:\\Users\\Hesh\\AppData\\Local\\Discord\\app-1.0.9164\\Discord.exe', 950, 540, 880, 120, 60, 640, 95, 9.1, 30],
  ['obs64.exe', 'C:\\Program Files\\obs-studio\\bin\\64bit\\obs64.exe', 3100, 410, 520, 40, 5200, 12, 2400, 1.9, 14],
  ['steam.exe', 'C:\\Program Files (x86)\\Steam\\steam.exe', 400, 180, 260, 1800, 2300, 4300, 60, 8.2, 30],
  ['explorer.exe', 'C:\\Windows\\explorer.exe', 380, 140, 210, 300, 45, 1, 0.5, 23.5, 30],
  ['MsMpEng.exe', null, 1700, 210, 330, 4100, 220, 30, 8, 24, 30],
  ['SearchIndexer.exe', 'C:\\Windows\\System32\\SearchIndexer.exe', 640, 48, 96, 1200, 380, 0, 0, 24, 30],
  ['WindowsTerminal.exe', 'C:\\Program Files\\WindowsApps\\Microsoft.WindowsTerminal_1.21.2361.0_x64__8wekyb3d8bbwe\\WindowsTerminal.exe', 120, 88, 140, 20, 6, 0, 0, 4.4, 18],
  ['orbit.exe', 'C:\\Users\\Hesh\\AppData\\Local\\Orbit\\orbit.exe', 210, 92, 120, 15, 30, 0.4, 0.1, 10.5, 6],
  ['Spotify.exe', 'C:\\Users\\Hesh\\AppData\\Roaming\\Spotify\\Spotify.exe', 520, 260, 340, 140, 380, 920, 18, 5.6, 25],
];

let cleared = false;

export default {
  async get_history({ days }, ctx) {
    await ctx.delay(120);
    if (cleared) return [];
    const d = Math.min(90, Math.max(1, Math.round(Number(days) || 30)));
    const now = Math.floor(Date.now() / 1000);
    return APPS.map(([name, exe, cpu, avg, max, rd, wr, rx, tx, hours, first]) => {
      const span = Math.min(d, first);
      return {
        key: (exe || name).toLowerCase(),
        name,
        exe,
        cpuSeconds: Math.round(cpu * span),
        maxMemory: Math.round(max * MB),
        avgMemory: Math.round(avg * MB),
        diskReadBytes: Math.round(rd * MB * span),
        diskWriteBytes: Math.round(wr * MB * span),
        // Per-app network needs administrator rights: 0 without it, as in the real app.
        netRxBytes: ctx.isAdmin() ? Math.round(rx * MB * span) : 0,
        netTxBytes: ctx.isAdmin() ? Math.round(tx * MB * span) : 0,
        activeSeconds: Math.round(hours * 3600 * span),
        firstSeen: now - first * DAY,
        lastSeen: now - (name === 'Adobe Premiere Pro.exe' ? 0 : 60),
      };
    });
  },

  clear_history() {
    cleared = true;
    return null;
  },

  set_history_enabled() {
    return null;
  },
};
