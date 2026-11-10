// Orbit / mock: Optimize > Debloat (src-tauri/src/debloat/) for the browser preview. Same
// shapes as Rust. A fixed PC with every verdict in every group; applying and undoing change
// only this module's state. A removed app's Undo "opens the Store"; the next list counts it as
// reinstalled, as if you installed it there.

const now = () => Math.floor(Date.now() / 1000);
const STORE = (family) => `ms-windows-store://pdp/?PFN=${family}`;
const CHANGE = { startup: 'Turn off at sign-in', task: 'Turn off the task', service: 'Set to Manual', app: 'Remove for this user' };
const START = { auto: 'Automatic', autoDelayed: 'Automatic (delayed)', manual: 'Manual', disabled: 'Disabled' };
const TITLES = { startup: 'Startup apps', task: 'Scheduled tasks', service: 'Services', app: 'Built-in apps' };

// [group, key, plain name, raw, verdict, what, note, extra]
const DEFS = [
  ['startup', 'HKCU\\Run\\Steam', 'Steam', 'Steam', 'off', 'Opens Steam in the background when you sign in.', 'Steam opens when you start it; game updates wait until then.', { impact: 'high', detail: '"C:\\Program Files (x86)\\Steam\\steam.exe" -silent', search: 'steam.exe' }],
  ['startup', 'HKCU\\Run\\Spotify', 'Spotify', 'Spotify', 'off', 'Opens Spotify, minimized, when you sign in.', 'Spotify opens when you start it.', { impact: 'medium', detail: '"C:\\Users\\you\\AppData\\Roaming\\Spotify\\Spotify.exe" --autostart --minimized', search: 'spotify.exe' }],
  ['startup', 'HKCU\\Run\\MicrosoftEdgeAutoLaunch_7E6C93263218', 'Microsoft Edge in the background', 'MicrosoftEdgeAutoLaunch_7E6C93263218', 'off', 'Starts Edge hidden at sign-in so it opens faster later.', 'Edge takes a moment longer to open the first time.', { impact: 'low', detail: '"C:\\Program Files (x86)\\Microsoft\\Edge\\Application\\msedge.exe" --no-startup-window', search: 'msedge.exe' }],
  ['startup', 'HKCU\\Run\\EpicGamesLauncher', 'Epic Games Launcher', 'EpicGamesLauncher', 'off', 'Opens the Epic launcher when you sign in.', 'The launcher opens when you start it; game updates wait until then.', { on: false, detail: '"C:\\Program Files\\Epic Games\\Launcher\\Portal\\Binaries\\Win64\\EpicGamesLauncher.exe" -silent', search: 'epicgameslauncher.exe' }],
  ['startup', 'HKCU\\Run\\OneDrive', 'Microsoft OneDrive', 'OneDrive', 'keep', 'Keeps your files in sync with OneDrive.', 'Turning it off pauses file sync and backup until you open OneDrive.', { detail: '"C:\\Users\\you\\AppData\\Local\\Microsoft\\OneDrive\\OneDrive.exe" /background', search: 'onedrive.exe' }],
  ['startup', 'HKLM\\Run\\SecurityHealth', 'Windows Security icon', 'SecurityHealth', 'keep', 'Shows the Windows Security icon and its alerts.', 'You would miss security alerts.', { guarded: true, admin: true, detail: '%windir%\\system32\\SecurityHealthSystray.exe', search: 'securityhealthsystray.exe' }],
  ['startup', 'HKCU\\Run\\Synapse3', 'Synapse3', 'Synapse3', 'unknown', null, null, { impact: 'high', suggested: true, detail: '"C:\\Program Files (x86)\\Razer\\Synapse3\\Razer Synapse 3.exe" /StartMinimized', search: 'Razer Synapse 3.exe' }],
  ['startup', 'UserFolder\\ShareX.lnk', 'ShareX', 'ShareX', 'unknown', null, null, { impact: 'low', detail: 'C:\\Program Files\\ShareX\\ShareX.exe -silent', search: 'ShareX.exe' }],
  ['startup', 'HKCU\\Run\\Notion', 'electron.app.Notion', 'electron.app.Notion', 'unknown', null, null, { detail: 'C:\\Users\\you\\AppData\\Local\\Programs\\Notion\\Notion.exe --open-at-login', search: 'Notion.exe' }],

  ['task', '\\Microsoft\\Windows\\Application Experience\\Microsoft Compatibility Appraiser', 'Windows compatibility check', '\\Microsoft\\Windows\\Application Experience\\Microsoft Compatibility Appraiser', 'off', 'Scans your apps and sends compatibility data to Microsoft before big updates.', 'Windows may not warn you early about an app that won\u2019t run after a big update.', { admin: true, detail: 'C:\\Windows\\system32\\compattelrunner.exe', search: 'Microsoft Compatibility Appraiser' }],
  ['task', '\\Microsoft\\Windows\\Customer Experience Improvement Program\\Consolidator', 'Windows usage reports', '\\Microsoft\\Windows\\Customer Experience Improvement Program\\Consolidator', 'off', 'Send Windows and USB usage data to Microsoft\u2019s improvement program.', 'Nothing you will notice.', { admin: true, detail: 'C:\\Windows\\system32\\wsqmcons.exe', search: 'Consolidator' }],
  ['task', '\\ZoomUpdateTaskUser-S-1-5-21-1001', 'Zoom updater', '\\ZoomUpdateTaskUser-S-1-5-21-1001', 'off', 'Checks for Zoom updates in the background.', 'Zoom checks for updates when you open it.', { detail: 'C:\\Users\\you\\AppData\\Roaming\\Zoom\\bin\\installer.exe', search: 'ZoomUpdateTaskUser-S-1-5-21-1001' }],
  ['task', '\\MicrosoftEdgeUpdateTaskMachineCore', 'Microsoft Edge updater', '\\MicrosoftEdgeUpdateTaskMachineCore', 'keep', 'Checks for Edge updates.', 'Edge gets security fixes through it, and Windows uses parts of Edge.', { admin: true, detail: 'C:\\Program Files (x86)\\Microsoft\\EdgeUpdate\\MicrosoftEdgeUpdate.exe', search: 'MicrosoftEdgeUpdateTaskMachineCore' }],
  ['task', '\\GoogleUser\\GoogleUpdater\\GoogleUpdaterTaskUser157.0.8089.2', 'Google updater', '\\GoogleUser\\GoogleUpdater\\GoogleUpdaterTaskUser157.0.8089.2', 'keep', 'Checks for Chrome and other Google app updates.', 'Chrome gets security fixes through it.', { detail: 'C:\\Users\\you\\AppData\\Local\\Google\\GoogleUpdater\\updater.exe', search: 'GoogleUpdaterTaskUser157.0.8089.2' }],
  ['task', '\\Driver Booster Scheduler', 'Driver Booster Scheduler', '\\Driver Booster Scheduler', 'unknown', null, null, { detail: 'C:\\Program Files (x86)\\IObit\\Driver Booster\\AutoUpdate.exe', search: 'Driver Booster Scheduler' }],
  ['task', '\\MSIAfterburner', 'MSIAfterburner', '\\MSIAfterburner', 'unknown', null, null, { admin: true, detail: 'C:\\Program Files (x86)\\MSI Afterburner\\MSIAfterburner.exe', search: 'MSIAfterburner' }],

  ['service', 'DiagTrack', 'Windows telemetry', 'DiagTrack', 'off', 'Sends diagnostic and usage data to Microsoft.', 'Nothing you will notice; Windows works the same.', { admin: true, start: 'auto', search: 'Connected User Experiences and Telemetry' }],
  ['service', 'MapsBroker', 'Downloaded maps manager', 'MapsBroker', 'off', 'Keeps offline maps up to date.', 'Offline maps update when you open Maps.', { admin: true, start: 'autoDelayed', search: 'Downloaded Maps Manager' }],
  ['service', 'dmwappushservice', 'Device management messages', 'dmwappushservice', 'off', 'Passes device-management and diagnostic messages.', 'A PC run by work or school may need it.', { admin: true, start: 'manual', search: 'Device Management Wireless Application Protocol (WAP) Push message Routing Service' }],
  ['service', 'SysMain', 'App preloader (SysMain)', 'SysMain', 'keep', 'Learns which apps you use and loads them ahead of time.', 'Apps open faster with it; turning it off rarely helps.', { admin: true, start: 'auto', search: 'SysMain' }],
  ['service', 'WSearch', 'Windows Search indexer', 'WSearch', 'keep', 'Indexes your files so Start and File Explorer find them fast.', 'Search gets slow or misses files without it.', { admin: true, start: 'autoDelayed', search: 'Windows Search' }],
  ['service', 'WinDefend', 'Microsoft Defender Antivirus Service', 'WinDefend', 'keep', null, 'It protects this PC.', { admin: true, guarded: true, start: 'auto', search: 'Microsoft Defender Antivirus Service' }],
  ['service', 'Razer Game Manager Service 3', 'Razer Game Manager Service 3', 'Razer Game Manager Service 3', 'unknown', null, null, { admin: true, start: 'auto', search: 'Razer Game Manager Service 3' }],
  ['service', 'CloudflareWARP', 'Cloudflare WARP', 'CloudflareWARP', 'unknown', null, null, { admin: true, start: 'auto', search: 'Cloudflare WARP' }],

  ['app', 'Clipchamp.Clipchamp_yxz26nhyzhsrt', 'Clipchamp', 'Clipchamp.Clipchamp', 'off', 'Microsoft\u2019s video editor.', 'The Clipchamp app.', { detail: 'Microsoft Clipchamp' }],
  ['app', 'Microsoft.BingNews_8wekyb3d8bbwe', 'Microsoft News', 'Microsoft.BingNews', 'off', 'A news headlines app.', 'The News app; news sites still open in your browser.', { detail: 'Microsoft News' }],
  ['app', 'Microsoft.BingWeather_8wekyb3d8bbwe', 'Weather', 'Microsoft.BingWeather', 'off', 'A weather forecast app.', 'The Weather app.', { detail: 'MSN Weather' }],
  ['app', 'Microsoft.MicrosoftSolitaireCollection_8wekyb3d8bbwe', 'Solitaire Collection', 'Microsoft.MicrosoftSolitaireCollection', 'off', 'Card games with ads.', 'The card games.', { detail: 'Microsoft Solitaire Collection' }],
  ['app', 'Microsoft.Getstarted_8wekyb3d8bbwe', 'Tips', 'Microsoft.Getstarted', 'off', 'Tips about Windows features.', 'The Tips app.', { detail: 'Tips' }],
  ['app', 'Microsoft.WindowsFeedbackHub_8wekyb3d8bbwe', 'Feedback Hub', 'Microsoft.WindowsFeedbackHub', 'off', 'Sends your feedback to Microsoft.', 'You can\u2019t send feedback to Microsoft.', { detail: 'Feedback Hub' }],
  ['app', 'king.com.CandyCrushSaga_kgqvnymyfvs32', 'Candy Crush and other King games', 'king.com.CandyCrushSaga', 'off', 'Games that come pre-installed.', 'The game and its progress on this PC.', { detail: 'Candy Crush Saga' }],
  ['app', 'Microsoft.XboxGamingOverlay_8wekyb3d8bbwe', 'Xbox Game Bar', 'Microsoft.XboxGamingOverlay', 'off', 'The Win+G overlay for screenshots, clips and chat.', 'Game Bar and its screenshots and clips.', { detail: 'Game Bar' }],
  ['app', 'Microsoft.WindowsStore_8wekyb3d8bbwe', 'Microsoft Store', 'Microsoft.WindowsStore', 'keep', 'Installs and updates Store apps.', 'Removed apps come back through it.', { guarded: true, detail: 'Microsoft Store' }],
  ['app', 'Microsoft.GamingServices_8wekyb3d8bbwe', 'Gaming Services', 'Microsoft.GamingServices', 'keep', 'Runs Game Pass and Microsoft Store games.', 'Those games won\u2019t start without it.', { detail: 'Gaming Services' }],
  ['app', 'Microsoft.HEIFImageExtension_8wekyb3d8bbwe', 'Picture and video format support', 'Microsoft.HEIFImageExtension', 'keep', 'Lets Windows open HEIF, WebP and RAW pictures and play VP9, AV1 and HEVC video.', 'Photos, video apps and the browser use it to open these files.', { guarded: true, detail: 'HEIF Image Extensions' }],
];

function fresh() {
  return DEFS.map(([group, key, name, raw, verdict, what, note, x]) => ({
    group, key, name, raw, verdict, what, note,
    on: x.start ? x.start === 'auto' || x.start === 'autoDelayed' : x.on !== false,
    start: x.start || null,
    needsAdmin: group === 'service' ? true : !!x.admin,
    guarded: !!x.guarded,
    impact: x.impact || null,
    suggested: !!x.suggested,
    detail: x.start ? `Starts: ${START[x.start]}` : x.detail || null,
    search: x.search || (group === 'app' ? x.detail || raw : raw),
    removed: false,
  }));
}

const state = { items: fresh(), changes: [], nextId: 1 };

function toItem(d) {
  const actionable = d.verdict === 'off' && d.on && !d.guarded;
  return {
    id: `${d.group}:${d.key}`,
    group: d.group,
    name: d.name,
    raw: d.raw,
    what: d.what,
    verdict: d.verdict,
    note: d.note,
    on: d.on,
    actionable,
    needsAdmin: d.needsAdmin,
    change: actionable ? CHANGE[d.group] : null,
    detail: d.detail,
    impact: d.impact,
    suggested: d.suggested && d.verdict === 'unknown',
    search: d.search,
    family: d.group === 'app' ? d.key : null,
    guarded: d.guarded,
  };
}

function counts(items) {
  const c = { off: 0, keep: 0, unknown: 0, ready: 0 };
  for (const it of items) {
    c[it.verdict] += 1;
    if (it.actionable) c.ready += 1;
  }
  return c;
}

const RANK = (it) => (it.verdict === 'off' ? (it.actionable ? 0 : 1) : it.verdict === 'keep' ? 2 : 3);

function analysis(ctx) {
  const groups = ['startup', 'task', 'service', 'app'].map((g) => {
    const items = state.items.filter((d) => d.group === g && !d.removed).map(toItem)
      .sort((a, b) => RANK(a) - RANK(b) || a.name.localeCompare(b.name));
    return { id: g, title: TITLES[g], counts: counts(items), items };
  });
  const total = { off: 0, keep: 0, unknown: 0, ready: 0 };
  for (const g of groups) for (const k of Object.keys(total)) total[k] += g.counts[k];
  return { readAt: now(), isAdmin: ctx.isAdmin(), counts: total, groups, errors: [] };
}

function plan(ids, ctx) {
  const admin = ctx.isAdmin();
  const changes = [];
  const refused = [];
  for (const id of [...new Set(ids || [])]) {
    const d = state.items.find((x) => `${x.group}:${x.key}` === id && !x.removed);
    if (!d) { refused.push({ id, name: id, reason: 'It is no longer on this PC. Analyze again.', needsAdmin: false }); continue; }
    const deny = (reason, needsAdmin = false) => refused.push({ id, name: d.name, reason, needsAdmin });
    if (d.guarded) { deny(d.note || 'Windows needs it to run.'); continue; }
    if (d.verdict !== 'off') { deny('Orbit only changes items marked Safe to turn off.'); continue; }
    if (!d.on) { deny('It is already off.'); continue; }
    if (d.needsAdmin && !admin) { deny('Needs administrator rights.', true); continue; }
    const [from, to] = d.group === 'startup' ? ['On at sign-in', 'Off']
      : d.group === 'task' ? ['On', 'Off']
        : d.group === 'service' ? [START[d.start], 'Manual'] : ['Installed', 'Removed for you'];
    changes.push({ id, group: d.group, name: d.name, raw: d.raw, action: CHANGE[d.group], from, to, needsAdmin: d.needsAdmin, def: d });
  }
  return { changes, refused, isAdmin: admin };
}

const newestFirst = () => [...state.changes].reverse().map((c) => ({ ...c }));

function revert(c) {
  const d = state.items.find((x) => `${x.group}:${x.key}` === c.item);
  if (!d) return;
  if (c.group === 'service') {
    d.start = c.before;
    d.on = true;
    d.detail = `Starts: ${START[c.before]}`;
  } else {
    d.on = true;
    d.removed = false;
  }
}

function undoOne(c, ctx) {
  if (c.needsAdmin && c.group !== 'app' && !ctx.isAdmin()) throw 'Needs administrator rights. Restart Orbit as administrator, then undo.';
  if (c.group === 'app') {
    c.storeOpened = true; // the next list counts it as reinstalled
    return false;
  }
  revert(c);
  return true;
}

export default {
  async debloat_analyze(_args, ctx) {
    await ctx.delay(700);
    return analysis(ctx);
  },

  async debloat_preview({ ids }, ctx) {
    if (!ids?.length) throw 'Tick at least one item first.';
    await ctx.delay(150);
    const p = plan(ids, ctx);
    return { ...p, changes: p.changes.map(({ def, ...rest }) => rest) };
  },

  async debloat_apply({ ids }, ctx) {
    if (!ids?.length) throw 'Tick at least one item first.';
    await ctx.delay(600);
    const p = plan(ids, ctx);
    const applied = [];
    for (const ch of p.changes) {
      const d = ch.def;
      const c = {
        id: state.nextId++, item: ch.id, group: d.group, name: d.name, raw: d.raw,
        target: d.group === 'app' ? `${d.raw}_1.0.0.0_x64__${d.key.split('_')[1]}` : d.key,
        action: ch.action, from: ch.from, to: ch.to,
        before: d.group === 'service' ? d.start : d.group === 'app' ? 'installed' : 'enabled',
        family: d.group === 'app' ? d.key : null, storeUrl: d.group === 'app' ? STORE(d.key) : null,
        needsAdmin: d.needsAdmin, at: now(), state: 'applied', undoneAt: null,
      };
      state.changes.push(c);
      if (d.group === 'service') {
        d.start = 'manual';
        d.on = false;
        d.detail = 'Starts: Manual';
      } else if (d.group === 'app') {
        d.removed = true;
      } else {
        d.on = false;
      }
      applied.push({ ...c });
    }
    return { applied, failed: [], refused: p.refused, changes: newestFirst() };
  },

  async debloat_changes() {
    for (const c of state.changes) {
      if (c.group === 'app' && c.state !== 'undone' && c.storeOpened) {
        c.state = 'undone';
        c.undoneAt = now();
        revert(c);
      }
    }
    return newestFirst();
  },

  async debloat_undo({ id }, ctx) {
    await ctx.delay(200);
    const c = state.changes.find((x) => x.id === id);
    if (!c) throw 'Orbit has no record of that change.';
    if (c.state === 'undone') throw 'That change was already undone.';
    if (undoOne(c, ctx)) {
      c.state = 'undone';
      c.undoneAt = now();
    }
    return { ...c };
  },

  async debloat_undo_all(_args, ctx) {
    await ctx.delay(400);
    const undone = [];
    const failed = [];
    const storeApps = [];
    for (const c of [...state.changes].reverse()) {
      if (c.state === 'undone') continue;
      if (c.group === 'app') { storeApps.push({ ...c }); continue; }
      try {
        undoOne(c, ctx);
        c.state = 'undone';
        c.undoneAt = now();
        undone.push({ ...c });
      } catch (err) {
        failed.push({ id: c.item, name: c.name, error: String(err) });
      }
    }
    return { undone, failed, storeApps, changes: newestFirst() };
  },
};
