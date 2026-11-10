// Orbit / mock: PC health report and startup suggestions (src-tauri/src/health.rs) for the
// browser preview. Mirrors the Rust rules on the mock startup entries and security scan.
// Windows Update starts "checking" and finishes after a few seconds, like the real search.

const GB = 1024 ** 3;
const MB = 1024 ** 2;

// Measured first-minute cost per mock startup entry (impact.rs numbers).
const COST = {
  Discord: { cpuMs: 4200, diskReadBytes: 18 * MB },
  'Adobe CCXProcess': { cpuMs: 2600, diskReadBytes: 9 * MB },
  'OBS Studio': { cpuMs: 700, diskReadBytes: 5 * MB },
  Spotify: { cpuMs: 650, diskReadBytes: 1.2 * MB },
  OneDrive: { cpuMs: 1800, diskReadBytes: 12 * MB },
  SecurityHealth: { cpuMs: 90, diskReadBytes: 140 * 1024 },
  RtkAudUService: { cpuMs: 120, diskReadBytes: 200 * 1024 },
};
const PUBLISHER = {
  Discord: 'Discord Inc.',
  'Adobe CCXProcess': 'Adobe Inc.',
  'OBS Studio': 'OBS Project',
  Spotify: 'Spotify AB',
  OneDrive: 'Microsoft Corporation',
  SecurityHealth: 'Microsoft Windows',
  RtkAudUService: 'Realtek Semiconductor Corp.',
};
const SECURITY_WORDS = ['antivirus', 'security', 'defender', 'malwarebytes', 'mcafee', 'norton', 'avast', 'kaspersky', 'eset', 'bitdefender'];

const now = () => Math.floor(Date.now() / 1000);
const BOOT = now() - (3 * 86400 + 5 * 3600);

let update = {
  state: 'idle', lastSearch: null, lastInstall: null, pending: null, pendingTitles: [],
  checkedAt: null, online: false, error: null,
};
let updateTimer = null;

function startUpdateCheck(online) {
  if (update.state === 'checking') return;
  update = { ...update, state: 'checking' };
  clearTimeout(updateTimer);
  updateTimer = setTimeout(() => {
    const t = now();
    update = {
      state: 'ready',
      lastSearch: online ? t : t - 2 * 86400,
      lastInstall: t - 9 * 86400,
      pending: 2,
      pendingTitles: [
        '2026-11 Cumulative Update for Windows 10 Version 22H2 for x64-based Systems (KB5072033)',
        'Security Intelligence Update for Microsoft Defender Antivirus - KB2267602 (Version 1.421.112.0)',
      ],
      checkedAt: t,
      online,
      error: null,
    };
  }, online ? 6000 : 3000);
}

const plural = (n, one, many) => `${n} ${n === 1 ? one : many}`;
const gb = (b) => (b / GB >= 10 ? `${Math.round(b / GB)} GB` : `${(b / GB).toFixed(1)} GB`);
const item = (key, title, status, text, extra = {}) => ({ key, title, status, text, detail: null, action: null, ...extra });
const act = (label, kind, target) => ({ label, kind, target });

function spaceStatus(total, available) {
  const warn = Math.min(Math.max(total / 10, 5 * GB), 25 * GB);
  const problem = Math.min(Math.max(total * 0.03, GB), 5 * GB);
  return available < problem ? 'problem' : available < warn ? 'warning' : 'good';
}

function isMicrosoft(path, publisher) {
  return /^c:\\windows\\/i.test(path || '') || /^Microsoft/.test(publisher || '');
}

function isSecurity(name, path, publisher) {
  const hay = `${name} ${path || ''} ${publisher || ''}`.toLowerCase();
  return SECURITY_WORDS.some((w) => hay.includes(w));
}

async function suggestions(ctx) {
  const list = await ctx.call('list_startup');
  const out = [];
  for (const e of list) {
    if (!e.enabled || !e.path || !e.impact || e.impact === 'low') continue;
    const c = COST[e.name];
    if (!c) continue;
    const publisher = PUBLISHER[e.name] || null;
    if (isMicrosoft(e.path, publisher) || isSecurity(e.name, e.path, publisher) || /^orbit$/i.test(e.name)) continue;
    out.push({
      id: e.id, name: e.name, path: e.path, impact: e.impact, cpuMs: c.cpuMs,
      diskReadBytes: Math.round(c.diskReadBytes), publisher, needsAdmin: e.needsAdmin,
      measuredAt: e.impactMeasuredAt,
      cost: c.cpuMs / 1000 + c.diskReadBytes / (3 * MB),
    });
  }
  out.sort((a, b) => b.cost - a.cost || a.name.localeCompare(b.name));
  return out.slice(0, 3).map(({ cost, ...s }) => s);
}

function drives() {
  const d = [
    { mount: 'C:\\', name: '', fileSystem: 'NTFS', total: 476 * GB, available: 61.4 * GB, removable: false },
    { mount: 'D:\\', name: 'Data', fileSystem: 'NTFS', total: 931 * GB, available: 18.2 * GB, removable: false },
    { mount: 'F:\\', name: 'USB', fileSystem: 'exFAT', total: 58 * GB, available: 31 * GB, removable: true },
  ];
  return d.map((x) => ({ ...x, total: Math.round(x.total), available: Math.round(x.available), status: spaceStatus(x.total, x.available) }));
}

function disks(admin) {
  const counters = (wear, temp, hours) => (admin
    ? { wearPct: wear, temperatureC: temp, powerOnHours: hours, readErrors: 0, writeErrors: 0 }
    : { wearPct: null, temperatureC: null, powerOnHours: null, readErrors: null, writeErrors: null });
  return [
    { deviceId: '0', name: 'Samsung SSD 980 PRO 500GB', mediaType: 'SSD', busType: 'NVMe', size: 500107862016, health: 'healthy', problems: [], drives: ['C:'], ...counters(6, 41, 8123) },
    { deviceId: '1', name: 'WDC WD10EZEX-08WN4A0', mediaType: 'HDD', busType: 'SATA', size: 1000204886016, health: 'healthy', problems: [], drives: ['D:'], ...counters(null, 36, 21450) },
  ];
}

function updatesItem(u) {
  const t = now();
  const settings = act('Open Windows Update', 'settings', 'ms-settings:windowsupdate');
  if (u.state === 'idle' || (u.state === 'checking' && u.pending == null)) {
    return item('updates', 'Windows Update', 'unknown', 'Checking Windows Update. This can take a minute.');
  }
  if (u.state === 'error' && u.pending == null) return item('updates', 'Windows Update', 'unknown', u.error, { action: settings });
  if (u.pending > 0) {
    return item('updates', 'Windows Update', 'warning', `${plural(u.pending, 'update is', 'updates are')} waiting to install.`, {
      detail: u.pendingTitles.join('; '), action: settings,
    });
  }
  const days = Math.floor((t - u.lastSearch) / 86400);
  return item('updates', 'Windows Update', 'good', `No updates are waiting. Windows last checked for updates ${days === 0 ? 'today' : days === 1 ? 'yesterday' : `${days} days ago`}.`);
}

export default {
  async get_health_report(args, ctx) {
    await ctx.delay(500);
    if (update.state === 'idle') startUpdateCheck(false);
    const admin = ctx.isAdmin();
    const dr = drives();
    const dk = disks(admin);
    const diskNote = admin ? null : 'Restart as administrator to see wear, temperature and error counts.';
    const startup = await ctx.call('list_startup');
    const picks = await suggestions(ctx);
    let findings = [];
    let scanError = null;
    try {
      findings = await ctx.call('security_scan');
    } catch (err) {
      scanError = String(err);
    }

    const fixed = dr.filter((d) => !d.removable);
    const low = fixed.filter((d) => d.status !== 'good');
    const worst = low.some((d) => d.status === 'problem') ? 'problem' : low.length ? 'warning' : 'good';
    const lowText = low.map((d) => `${d.mount.replace(/\\$/, '')} (${gb(d.available)} free)`).join(', ');
    const space = worst === 'good'
      ? item('space', 'Free space', 'good', `All ${fixed.length} drives have room to spare.`)
      : item('space', 'Free space', worst, worst === 'problem'
        ? `${lowText} is almost full. Windows and apps slow down or fail when a drive fills up.`
        : `${lowText} is getting full.`, { action: act('Free up space', 'optimize', 'cleanup') });

    const reasons = ['An app or update will replace some files at the next restart.'];
    const enabled = startup.filter((e) => e.enabled);
    const high = enabled.filter((e) => e.impact === 'high').length;
    const hi = findings.filter((f) => f.severity === 'high').length;
    const med = findings.filter((f) => f.severity === 'medium').length;
    const security = scanError
      ? item('security', 'Security scan', 'unknown', scanError)
      : hi
        ? item('security', 'Security scan', 'problem', `${plural(hi + med, 'program may', 'programs may')} need a close look (${hi} high, ${med} medium).`)
        : med
          ? item('security', 'Security scan', 'warning', `${plural(med, 'program is', 'programs are')} worth a look: unsigned or running from an unusual folder.`)
          : item('security', 'Security scan', 'good', 'Nothing suspicious among the running programs.');
    security.action = act('Go to Security', 'view', 'security');

    const drivers = await ctx.call('list_drivers');
    const bad = drivers.devices.filter((d) => d.flags.includes('problem'));
    const devices = bad.length
      ? item('devices', 'Devices', 'warning', bad.length === 1 ? `${bad[0].name} reports a problem.` : `${bad.length} devices report a problem.`, {
        detail: bad.slice(0, 5).map((d) => `${d.name}: ${d.problemText}`).join(' '), action: act('Go to Drivers', 'optimize', 'drivers'),
      })
      : item('devices', 'Devices', 'good', 'No device reports a problem.');

    const uptime = now() - BOOT;
    const items = [
      item('disks', 'Disk health', 'good', `All ${dk.length} disks report healthy.`, admin ? {} : { detail: diskNote, action: act('Restart as administrator', 'admin', '') }),
      space,
      item('restart', 'Restart', 'warning', 'A restart is pending. Restart when it suits you; Orbit will not restart the PC.', { detail: reasons.join(' ') }),
      updatesItem(update),
      devices,
      item('uptime', 'Uptime', 'good', `Restarted ${plural(Math.floor(uptime / 86400), 'day', 'days')} ago.`),
      item('startup', 'Startup load', high && picks.length ? 'warning' : 'good',
        high && picks.length
          ? `${plural(enabled.length, 'app starts', 'apps start')} with Windows. ${plural(high, 'has', 'have')} a high impact on sign-in.`
          : `${plural(enabled.length, 'app starts', 'apps start')} with Windows. None of them slows sign-in much.`,
        { action: act('Go to Startup', 'view', 'startup') }),
      security,
    ];
    return {
      generatedAt: now(), isAdmin: admin, items, drives: dr, disks: dk, diskNote,
      update: { ...update }, uptimeSecs: uptime, restartReasons: reasons,
    };
  },

  get_update_status({ refresh } = {}) {
    if (refresh) startUpdateCheck(true);
    else if (update.state === 'idle') startUpdateCheck(false);
    return { ...update };
  },

  get_startup_suggestions(args, ctx) {
    return suggestions(ctx);
  },

  open_settings_page({ page }) {
    if (!/^ms-settings:[a-z0-9-]+$/.test(String(page || ''))) throw 'Orbit can only open Windows Settings pages.';
    console.info(`[orbit mock] would open ${page}`);
    return null;
  },
};
