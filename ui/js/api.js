// Orbit / api: the Tauri bridge, plus a mock backend so the UI runs in a normal browser.
// In Tauri (withGlobalTauri) every call goes to the Rust commands; outside Tauri the mock
// below answers every command in docs/CONTRACT.md with realistic, drifting data. Commands
// added in v0.4 are answered by the feature mocks in js/mock/ (docs/V04-CONTRACT.md).

import * as mockRegistry from './mock/index.js';

const T = typeof window !== 'undefined' ? window.__TAURI__ : undefined;

export const isTauri = !!(T && T.core);

/** Invoke a backend command. Rejects with a short human error string on failure. */
export async function call(cmd, args = {}) {
  if (isTauri) return T.core.invoke(cmd, args);
  return mock.call(cmd, args || {});
}

/** Subscribe to orbit://snapshot. Returns an unsubscribe function. */
export function onSnapshot(cb) {
  if (isTauri) {
    let unlisten = null;
    let cancelled = false;
    T.event.listen('orbit://snapshot', (e) => cb(e.payload)).then((fn) => {
      if (cancelled) fn();
      else unlisten = fn;
    });
    return () => {
      cancelled = true;
      if (unlisten) unlisten();
    };
  }
  return mock.subscribe(cb);
}

function currentWindow() {
  return T && T.window && typeof T.window.getCurrentWindow === 'function' ? T.window.getCurrentWindow() : null;
}

export const win = {
  minimize() {
    const w = isTauri && currentWindow();
    return w ? w.minimize() : Promise.resolve();
  },
  toggleMaximize() {
    const w = isTauri && currentWindow();
    return w ? w.toggleMaximize() : Promise.resolve();
  },
  close() {
    const w = isTauri && currentWindow();
    return w ? w.close() : Promise.resolve();
  },
  startDragging() {
    const w = isTauri && currentWindow();
    return w ? w.startDragging() : Promise.resolve();
  },
  isMaximized() {
    const w = isTauri && currentWindow();
    return w ? w.isMaximized() : Promise.resolve(false);
  },
  setAlwaysOnTop(on) {
    const w = isTauri && currentWindow();
    return w ? w.setAlwaysOnTop(!!on) : Promise.resolve();
  },
};

/** The installed app's version (from tauri.conf.json). The browser preview reports MOCK_VERSION. */
export async function appVersion() {
  if (isTauri && T.app && typeof T.app.getVersion === 'function') return T.app.getVersion();
  return MOCK_VERSION;
}

const MOCK_VERSION = '0.4.0';

// ======================================================================== mock backend

const mock = (() => {
  const GB = 1024 ** 3;
  const MB = 1024 ** 2;
  const TOTAL_MEM = 16 * GB;
  const CORES = 8;
  const PHYSICAL = 4;
  const now = () => Date.now();
  const BOOT = Math.floor(now() / 1000) - (2 * 86400 + 5 * 3600 + 17 * 60);

  // Deterministic randomness so the preview looks the same on every load.
  let seed = 0x5eed1e;
  function rand() {
    seed |= 0;
    seed = (seed + 0x6d2b79f5) | 0;
    let t = Math.imul(seed ^ (seed >>> 15), 1 | seed);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  }
  const noise = () => (Math.random() * 2 - 1);
  // ?nogpu: no GPU counters, as on a PC without them. Rust is all-or-nothing: gpu.usage
  // and every row's gpu are null together, or all numbers.
  const NO_GPU = typeof location !== 'undefined' && new URLSearchParams(location.search).has('nogpu');
  const clamp = (v, lo, hi) => (v < lo ? lo : v > hi ? hi : v);

  const state = {
    isAdmin: false,
    intervalMs: 1000,
    paused: false,
    delta: false, // set_sampling({ delta: true }): emit SnapshotDelta like the Rust sampler
    deltaMode: false,
    emitted: new Map(), // pid -> startTime of the rows in the previous event
    timer: 0,
    listeners: new Set(),
    procs: new Map(), // pid -> internal proc
    last: null,
    tick: 0,
    netTotals: { Ethernet: [41.2 * GB, 3.9 * GB], 'Wi-Fi': [0, 0], 'vEthernet (WSL)': [212 * MB, 38 * MB] },
    netCur: { Ethernet: [0, 0], 'Wi-Fi': [0, 0], 'vEthernet (WSL)': [0, 0] },
    threads: 3420,
    handles: 148200,
    swapUsed: 0.62 * GB,
    cached: 3.4 * GB,
    freq: 4200,
    transientPid: null,
    transientUntil: 0,
    ownPid: null, // the mock orbit.exe, reported as StaticInfo.pid
  };

  // ------------------------------------------------------------ process templates

  const SYS32 = 'C:\\Windows\\System32';
  // 9172 and 11408: the two malware-like files mock/security.js reports as high findings.
  const usedPids = new Set([0, 4, 9172, 11408]);
  function newPid() {
    let p;
    do {
      p = 4 * (40 + Math.floor(rand() * 6400));
    } while (usedPids.has(p));
    usedPids.add(p);
    return p;
  }

  const CORE_NAMES = new Set([
    'system', 'registry', 'smss.exe', 'csrss.exe', 'wininit.exe', 'services.exe', 'lsass.exe',
    'svchost.exe', 'winlogon.exe', 'dwm.exe', 'fontdrvhost.exe', 'memory compression', 'secure system',
  ]);
  const PROTECTED = new Set([
    'system', 'registry', 'smss.exe', 'csrss.exe', 'wininit.exe', 'services.exe', 'lsass.exe',
    'memory compression', 'msmpeng.exe', 'nissrv.exe', 'sgrmbroker.exe',
  ]);

  function kindOf(p) {
    if (p.windowTitle) return 'app';
    const n = p.name.toLowerCase();
    if (p.pid === 0 || p.pid === 4 || CORE_NAMES.has(n)) return 'windows';
    if (p.exe && p.exe.toLowerCase().startsWith('c:\\windows\\')) return 'windows';
    return 'background';
  }

  function addProc(t) {
    const pid = t.pid ?? newPid();
    // Template CPU figures are relative weights; scale them to a calm, believable total.
    const cpuBase = (t.cpu ?? 0.05) * 0.5;
    const p = {
      pid,
      parentPid: t.parent ?? null,
      name: t.name,
      exe: t.exe ?? null,
      cmd: t.cmd ?? (t.exe ? `"${t.exe}"` : ''),
      user: t.user ?? 'Hesh',
      cpuBase,
      cpu: cpuBase,
      memBase: (t.mem ?? 20) * MB,
      memory: (t.mem ?? 20) * MB,
      ioBias: t.io ?? 0,
      diskReadBps: 0,
      diskWriteBps: 0,
      gpuBase: t.gpu ?? 0,
      gpu: NO_GPU ? null : (t.gpu ?? 0),
      status: 'running',
      startTime: t.start ?? (BOOT + 20 + Math.floor(rand() * 3600)),
      windowTitle: t.title ?? null,
      notResponding: !!t.hung,
      suspended: !!t.suspended,
      threads: t.threads ?? (4 + Math.floor(rand() * 30)),
      handles: t.handles ?? (120 + Math.floor(rand() * 900)),
      priority: t.priority ?? 'normal',
      cwd: t.cwd ?? (t.exe ? t.exe.slice(0, t.exe.lastIndexOf('\\')) : null),
      virt: (t.mem ?? 20) * MB * (2.5 + rand() * 6),
      totalRead: Math.floor(rand() * 900) * MB,
      totalWritten: Math.floor(rand() * 300) * MB,
      kind: 'background',
    };
    p.kind = kindOf(p);
    state.procs.set(pid, p);
    return p;
  }

  function seedProcesses() {
    const at = (secs) => BOOT + secs;
    addProc({ pid: 4, name: 'System', exe: null, cmd: '', user: 'SYSTEM', cpu: 0.4, mem: 0.15, start: at(0), threads: 268, handles: 6400 });
    const registry = addProc({ name: 'Registry', parent: 4, exe: null, cmd: '', user: 'SYSTEM', cpu: 0, mem: 62, start: at(0) });
    const smss = addProc({ name: 'smss.exe', parent: 4, exe: null, cmd: '', user: 'SYSTEM', cpu: 0, mem: 1.1, start: at(1) });
    addProc({ name: 'Memory Compression', parent: 4, exe: null, cmd: '', user: 'SYSTEM', cpu: 0.05, mem: 418, start: at(9) });
    void registry; void smss;
    const ghostSmss = 4 * 97; // exited parent, like on a real machine
    addProc({ name: 'csrss.exe', parent: ghostSmss, exe: null, cmd: '', user: 'SYSTEM', cpu: 0.1, mem: 5.2, start: at(2) });
    addProc({ name: 'csrss.exe', parent: 4 * 131, exe: null, cmd: '', user: 'SYSTEM', cpu: 0.3, mem: 6.8, start: at(3) });
    const wininit = addProc({ name: 'wininit.exe', parent: ghostSmss, exe: null, cmd: '', user: 'SYSTEM', cpu: 0, mem: 6.1, start: at(2) });
    const winlogon = addProc({ name: 'winlogon.exe', parent: 4 * 131, exe: `${SYS32}\\winlogon.exe`, cmd: 'winlogon.exe', user: 'SYSTEM', cpu: 0, mem: 9.4, start: at(3) });
    const services = addProc({ name: 'services.exe', parent: wininit.pid, exe: null, cmd: '', user: 'SYSTEM', cpu: 0.15, mem: 10.5, start: at(3) });
    addProc({ name: 'lsass.exe', parent: wininit.pid, exe: null, cmd: '', user: 'SYSTEM', cpu: 0.08, mem: 21, start: at(3) });
    addProc({ name: 'fontdrvhost.exe', parent: wininit.pid, exe: `${SYS32}\\fontdrvhost.exe`, cmd: '"fontdrvhost.exe"', user: 'UMFD-0', cpu: 0, mem: 3.4, start: at(3) });
    addProc({ name: 'fontdrvhost.exe', parent: winlogon.pid, exe: `${SYS32}\\fontdrvhost.exe`, cmd: '"fontdrvhost.exe"', user: 'UMFD-1', cpu: 0, mem: 9.8, start: at(3) });
    addProc({ name: 'dwm.exe', parent: winlogon.pid, exe: `${SYS32}\\dwm.exe`, cmd: '"dwm.exe"', user: 'DWM-1', cpu: 1.4, mem: 96, gpu: 4.5, start: at(4), priority: 'high' });

    // svchost x30: service host groups.
    const groups = [
      ['DcomLaunch', 'SYSTEM', 'DcomLaunch -p'], ['RPCSS', 'NETWORK SERVICE', 'RPCSS -p'],
      ['LocalServiceNetworkRestricted', 'LOCAL SERVICE', 'LocalServiceNetworkRestricted -p -s Dhcp'],
      ['netsvcs', 'SYSTEM', 'netsvcs -p -s Schedule'], ['netsvcs', 'SYSTEM', 'netsvcs -p -s Winmgmt'],
      ['NetworkService', 'NETWORK SERVICE', 'NetworkService -p -s Dnscache'], ['LocalService', 'LOCAL SERVICE', 'LocalService -p -s EventLog'],
      ['LocalSystemNetworkRestricted', 'SYSTEM', 'LocalSystemNetworkRestricted -p -s SysMain'], ['netsvcs', 'SYSTEM', 'netsvcs -p -s Themes'],
      ['LocalServiceNoNetworkFirewall', 'LOCAL SERVICE', 'LocalServiceNoNetworkFirewall -p'], ['netsvcs', 'SYSTEM', 'netsvcs -p -s ProfSvc'],
      ['LocalServiceNetworkRestricted', 'LOCAL SERVICE', 'LocalServiceNetworkRestricted -p -s lmhosts'], ['netsvcs', 'SYSTEM', 'netsvcs -p -s UserManager'],
      ['LocalService', 'LOCAL SERVICE', 'LocalService -s nsi'], ['LocalServiceNetworkRestricted', 'LOCAL SERVICE', 'LocalServiceNetworkRestricted -p -s AudioSrv'],
      ['netsvcs', 'SYSTEM', 'netsvcs -p -s BITS'], ['netsvcs', 'SYSTEM', 'netsvcs -p -s wuauserv'],
      ['NetworkService', 'NETWORK SERVICE', 'NetworkService -p -s CryptSvc'], ['LocalService', 'LOCAL SERVICE', 'LocalService -p -s W32Time'],
      ['LocalServiceNetworkRestricted', 'LOCAL SERVICE', 'LocalServiceNetworkRestricted -p -s WinHttpAutoProxySvc'], ['netsvcs', 'SYSTEM', 'netsvcs -p -s iphlpsvc'],
      ['LocalSystemNetworkRestricted', 'SYSTEM', 'LocalSystemNetworkRestricted -p -s TrkWks'], ['UnistackSvcGroup', 'Hesh', 'UnistackSvcGroup -s WpnUserService'],
      ['UnistackSvcGroup', 'Hesh', 'UnistackSvcGroup -s CDPUserSvc'], ['ClipboardSvcGroup', 'Hesh', 'ClipboardSvcGroup -p -s cbdhsvc'],
      ['LocalServiceNetworkRestricted', 'LOCAL SERVICE', 'LocalServiceNetworkRestricted -p -s WlanSvc'], ['netsvcs', 'SYSTEM', 'netsvcs -p -s LanmanServer'],
      ['NetworkService', 'NETWORK SERVICE', 'NetworkService -p -s LanmanWorkstation'], ['LocalSystemNetworkRestricted', 'SYSTEM', 'LocalSystemNetworkRestricted -p -s PcaSvc'],
      ['netsvcs', 'SYSTEM', 'netsvcs -p -s UsoSvc'],
    ];
    const svcPids = {};
    let dcom = null;
    groups.forEach(([, user, args], i) => {
      const p = addProc({
        name: 'svchost.exe', parent: services.pid, exe: `${SYS32}\\svchost.exe`,
        cmd: `C:\\Windows\\system32\\svchost.exe -k ${args}`, user,
        cpu: i === 4 ? 0.6 : i === 7 ? 0.3 : 0.02 + rand() * 0.12,
        mem: 4 + rand() * (i === 3 || i === 7 ? 60 : 24), start: at(4 + i), io: i === 7 ? 0.4 : 0,
      });
      const s = args.match(/-s (\S+)/);
      if (s) svcPids[s[1]] = p.pid;
      if (i === 0) dcom = p;
      if (i === 1) svcPids.RpcSs = p.pid;
      if (i === 9) svcPids.MpsSvc = p.pid;
    });

    const spool = addProc({ name: 'spoolsv.exe', parent: services.pid, exe: `${SYS32}\\spoolsv.exe`, cmd: `${SYS32}\\spoolsv.exe`, user: 'SYSTEM', cpu: 0, mem: 11, start: at(9) });
    svcPids.Spooler = spool.pid;
    const defender = addProc({ name: 'MsMpEng.exe', parent: services.pid, exe: null, cmd: '', user: 'SYSTEM', cpu: 0.8, mem: 214, start: at(10), io: 1.2 });
    svcPids.WinDefend = defender.pid;
    const nis = addProc({ name: 'NisSrv.exe', parent: services.pid, exe: null, cmd: '', user: 'LOCAL SERVICE', cpu: 0, mem: 9, start: at(14) });
    svcPids.WdNisSvc = nis.pid;
    addProc({ name: 'SgrmBroker.exe', parent: services.pid, exe: null, cmd: '', user: 'SYSTEM', cpu: 0, mem: 6.5, start: at(12) });
    const health = addProc({ name: 'SecurityHealthService.exe', parent: services.pid, exe: `${SYS32}\\SecurityHealthService.exe`, user: 'SYSTEM', cpu: 0, mem: 12, start: at(20) });
    svcPids.SecurityHealthService = health.pid;
    const indexer = addProc({ name: 'SearchIndexer.exe', parent: services.pid, exe: `${SYS32}\\SearchIndexer.exe`, cmd: `${SYS32}\\SearchIndexer.exe /Embedding`, user: 'SYSTEM', cpu: 0.2, mem: 48, start: at(22), io: 0.8 });
    svcPids.WSearch = indexer.pid;
    addProc({ name: 'audiodg.exe', parent: svcPids.AudioSrv, exe: `${SYS32}\\audiodg.exe`, cmd: `${SYS32}\\AUDIODG.EXE 0x4f8`, user: 'LOCAL SERVICE', cpu: 0.4, mem: 14, start: at(3200) });
    for (let i = 0; i < 2; i++) addProc({ name: 'WmiPrvSE.exe', parent: dcom.pid, exe: `${SYS32}\\wbem\\WmiPrvSE.exe`, user: i ? 'NETWORK SERVICE' : 'SYSTEM', cpu: 0.05, mem: 9 + i * 6 });
    for (let i = 0; i < 2; i++) addProc({ name: 'dllhost.exe', parent: dcom.pid, exe: `${SYS32}\\dllhost.exe`, cmd: `${SYS32}\\DllHost.exe /Processid:{AB8902B4-09CA-4BB6-B78D-A8F59079A8D5}`, cpu: 0, mem: 7 });
    const nvc = addProc({ name: 'NVDisplay.Container.exe', parent: services.pid, exe: 'C:\\Windows\\System32\\DriverStore\\FileRepository\\nv_dispi.inf_amd64_1f2a3b4c5d6e7f80\\Display.NvContainer\\NVDisplay.Container.exe', cmd: '-s NVDisplay.ContainerLocalSystem -f "C:\\ProgramData\\NVIDIA\\NVDisplay.ContainerLocalSystem.log"', user: 'SYSTEM', cpu: 0.05, mem: 22 });
    svcPids['NVDisplay.ContainerLocalSystem'] = nvc.pid;
    addProc({ name: 'NVDisplay.Container.exe', parent: nvc.pid, exe: nvc.exe, cmd: '-s NVDisplay.ContainerLocalSystem -f "C:\\ProgramData\\NVIDIA\\NVDisplay.ContainerLocalSystem.log" -l 3 -d', user: 'SYSTEM', cpu: 0.1, mem: 48 });
    for (let i = 0; i < 2; i++) addProc({ name: 'nvcontainer.exe', parent: services.pid, exe: 'C:\\Program Files\\NVIDIA Corporation\\NvContainer\\nvcontainer.exe', cmd: `"C:\\Program Files\\NVIDIA Corporation\\NvContainer\\nvcontainer.exe" -s NvContainerLocalSystem -r -p ${30000 + i}`, user: 'SYSTEM', cpu: 0.03, mem: 18 + i * 9 });
    const rtk = addProc({ name: 'RtkAudUService64.exe', parent: services.pid, exe: 'C:\\Windows\\System32\\DriverStore\\FileRepository\\realtekservice.inf_amd64_8c2b1a0f9e7d6c51\\RtkAudUService64.exe', user: 'SYSTEM', cpu: 0, mem: 7 });
    svcPids.RtkAudioUniversalService = rtk.pid;
    const steamSvc = addProc({ name: 'steamservice.exe', parent: services.pid, exe: 'C:\\Program Files (x86)\\Common Files\\Steam\\steamservice.exe', cmd: '"C:\\Program Files (x86)\\Common Files\\Steam\\steamservice.exe" /RunAsService', user: 'SYSTEM', cpu: 0, mem: 8 });
    svcPids['Steam Client Service'] = steamSvc.pid;

    // Signed-in user session.
    const explorer = addProc({ name: 'explorer.exe', parent: 4 * 233, exe: 'C:\\Windows\\explorer.exe', cmd: 'C:\\Windows\\Explorer.EXE', cpu: 0.6, mem: 142, gpu: 0.4, title: 'Downloads', start: at(31), threads: 88, handles: 4100 });
    addProc({ name: 'sihost.exe', parent: svcPids.UserManager, exe: `${SYS32}\\sihost.exe`, cmd: 'sihost.exe', cpu: 0.05, mem: 28, start: at(30) });
    addProc({ name: 'taskhostw.exe', parent: svcPids.Schedule, exe: `${SYS32}\\taskhostw.exe`, cmd: 'taskhostw.exe {222A245B-E637-4AE9-A93F-A59CA119A75E}', cpu: 0, mem: 15, start: at(30) });
    addProc({ name: 'ctfmon.exe', parent: svcPids.Schedule, exe: `${SYS32}\\ctfmon.exe`, cmd: '"ctfmon.exe"', cpu: 0.05, mem: 19, start: at(31) });
    for (let i = 0; i < 4; i++) addProc({ name: 'RuntimeBroker.exe', parent: dcom.pid, exe: `${SYS32}\\RuntimeBroker.exe`, cmd: `${SYS32}\\RuntimeBroker.exe -Embedding`, cpu: 0.02, mem: 8 + rand() * 30 });
    addProc({ name: 'SearchApp.exe', parent: dcom.pid, exe: 'C:\\Windows\\SystemApps\\Microsoft.Windows.Search_cw5n1h2txyewy\\SearchApp.exe', cmd: '"C:\\Windows\\SystemApps\\Microsoft.Windows.Search_cw5n1h2txyewy\\SearchApp.exe" -ServerName:CortanaUI.AppX8z9r6jm96hw4bsbneegw0kyxx296wr9t.mca', cpu: 0, mem: 168, start: at(40) });
    addProc({ name: 'StartMenuExperienceHost.exe', parent: dcom.pid, exe: 'C:\\Windows\\SystemApps\\Microsoft.Windows.StartMenuExperienceHost_cw5n1h2txyewy\\StartMenuExperienceHost.exe', cmd: '"C:\\Windows\\SystemApps\\Microsoft.Windows.StartMenuExperienceHost_cw5n1h2txyewy\\StartMenuExperienceHost.exe" -ServerName:App.AppXywbrabmsek0gm3tkwpr5kwzbs55tkqay.mca', cpu: 0, mem: 64, start: at(33) });
    addProc({ name: 'ShellExperienceHost.exe', parent: dcom.pid, exe: 'C:\\Windows\\SystemApps\\ShellExperienceHost_cw5n1h2txyewy\\ShellExperienceHost.exe', cmd: '"C:\\Windows\\SystemApps\\ShellExperienceHost_cw5n1h2txyewy\\ShellExperienceHost.exe" -ServerName:App.AppXtk181tbxbce2qsex02s8tw7hfxa9xb3t.mca', cpu: 0, mem: 52, start: at(34) });
    addProc({ name: 'TextInputHost.exe', parent: dcom.pid, exe: 'C:\\Windows\\SystemApps\\MicrosoftWindows.Client.CBS_cw5n1h2txyewy\\TextInputHost.exe', cmd: '"C:\\Windows\\SystemApps\\MicrosoftWindows.Client.CBS_cw5n1h2txyewy\\TextInputHost.exe" -ServerName:InputApp.AppX9jnwykgrccxc8by3hsrsh07r423xzvav.mca', cpu: 0.02, mem: 38, start: at(35) });
    addProc({ name: 'SecurityHealthSystray.exe', parent: explorer.pid, exe: `${SYS32}\\SecurityHealthSystray.exe`, cmd: `"${SYS32}\\SecurityHealthSystray.exe"`, cpu: 0, mem: 5, start: at(42) });
    addProc({ name: 'RtkAudUService64.exe', parent: rtk.pid, exe: rtk.exe, cmd: `"${rtk.exe}" -background`, cpu: 0, mem: 6, start: at(44) });
    addProc({ name: 'smartscreen.exe', parent: dcom.pid, exe: `${SYS32}\\smartscreen.exe`, cmd: `${SYS32}\\smartscreen.exe -Embedding`, cpu: 0, mem: 24 });
    addProc({ name: 'PhoneExperienceHost.exe', parent: dcom.pid, exe: 'C:\\Program Files\\WindowsApps\\Microsoft.YourPhone_1.24092.211.0_x64__8wekyb3d8bbwe\\PhoneExperienceHost.exe', cmd: '"C:\\Program Files\\WindowsApps\\Microsoft.YourPhone_1.24092.211.0_x64__8wekyb3d8bbwe\\PhoneExperienceHost.exe" -ComServer:Background', cpu: 0.02, mem: 41 });
    addProc({ name: 'OneDrive.exe', parent: explorer.pid, exe: 'C:\\Users\\Hesh\\AppData\\Local\\Microsoft\\OneDrive\\OneDrive.exe', cmd: '"C:\\Users\\Hesh\\AppData\\Local\\Microsoft\\OneDrive\\OneDrive.exe" /background', cpu: 0, mem: 58, suspended: true, start: at(48) });
    // Malware-like files, so the mock Security view's high findings point at live rows.
    addProc({ pid: 9172, name: 'svch0st.exe', parent: explorer.pid, exe: 'C:\\Users\\Hesh\\AppData\\Local\\Temp\\svch0st.exe', cpu: 0.1, mem: 14, start: at(50) });
    addProc({ pid: 11408, name: 'chromeupdate.exe', parent: explorer.pid, exe: 'C:\\Users\\Hesh\\AppData\\Local\\Google\\Chrome\\User Data\\Default\\chromeupdate.exe', cpu: 0.05, mem: 9, start: at(51) });

    // Apps.
    const chromeExe = 'C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe';
    const chrome = addProc({ name: 'chrome.exe', parent: explorer.pid, exe: chromeExe, cmd: `"${chromeExe}"`, cpu: 2.2, mem: 312, gpu: 0, title: 'Orbit design review - Google Chrome', start: at(5400), threads: 44, handles: 2300, io: 0.3 });
    const chromeKids = [
      ['--type=gpu-process --gpu-preferences=WAAAAAAAAADgAAAEAAAAAAAAAAAAAAAAAABgAAEAAAA4AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA --field-trial-handle=1984', 1.6, 260, 3.2],
      ['--type=utility --utility-sub-type=network.mojom.NetworkService --lang=en-US --service-sandbox-type=none', 0.4, 46, 0],
      ['--type=utility --utility-sub-type=storage.mojom.StorageService --lang=en-US --service-sandbox-type=service', 0.05, 22, 0],
      ['--type=renderer --renderer-client-id=7 --lang=en-US --num-raster-threads=4 --launch-time-ticks=812663542', 3.1, 388, 0],
      ['--type=renderer --renderer-client-id=9 --lang=en-US --num-raster-threads=4 --launch-time-ticks=814002217', 0.8, 214, 0],
      ['--type=renderer --renderer-client-id=12 --extension-process --lang=en-US --num-raster-threads=4', 0.1, 92, 0],
      ['--type=crashpad-handler --monitor-self-annotation=ptype=crashpad-handler --database=C:\\Users\\Hesh\\AppData\\Local\\Google\\Chrome\\User Data\\Crashpad', 0, 9, 0],
    ];
    chromeKids.forEach(([args, cpu, mem, gpu]) => addProc({ name: 'chrome.exe', parent: chrome.pid, exe: chromeExe, cmd: `"${chromeExe}" ${args}`, cpu, mem, gpu, start: at(5401 + Math.floor(rand() * 900)) }));

    const codeExe = 'C:\\Users\\Hesh\\AppData\\Local\\Programs\\Microsoft VS Code\\Code.exe';
    const code = addProc({ name: 'Code.exe', parent: explorer.pid, exe: codeExe, cmd: `"${codeExe}" "E:\\Work\\LegacyHub\\apps\\orbit"`, cpu: 1.1, mem: 168, title: 'app.js - orbit - Visual Studio Code', start: at(6100), io: 0.2, cwd: 'E:\\Work\\LegacyHub\\apps\\orbit' });
    [['--type=gpu-process', 0.7, 140, 1.4], ['--type=renderer', 2.4, 520, 0], ['--type=utility --utility-sub-type=node.mojom.NodeService', 1.2, 410, 0], ['--type=utility --utility-sub-type=network.mojom.NetworkService', 0.1, 36, 0]]
      .forEach(([args, cpu, mem, gpu]) => addProc({ name: 'Code.exe', parent: code.pid, exe: codeExe, cmd: `"${codeExe}" ${args} --lang=en-US`, cpu, mem, gpu, start: at(6101), cwd: 'E:\\Work\\LegacyHub\\apps\\orbit' }));

    const discordExe = 'C:\\Users\\Hesh\\AppData\\Local\\Discord\\app-1.0.9164\\Discord.exe';
    const discord = addProc({ name: 'Discord.exe', parent: explorer.pid, exe: discordExe, cmd: `"${discordExe}"`, cpu: 0.5, mem: 118, title: '#general | LegacyTech - Discord', start: at(52) });
    [['--type=gpu-process', 0.3, 96, 0.8], ['--type=renderer', 0.9, 286, 0], ['--type=utility --utility-sub-type=audio.mojom.AudioService', 0.2, 28, 0]]
      .forEach(([args, cpu, mem, gpu]) => addProc({ name: 'Discord.exe', parent: discord.pid, exe: discordExe, cmd: `"${discordExe}" ${args}`, cpu, mem, gpu, start: at(54) }));

    const spotifyExe = 'C:\\Users\\Hesh\\AppData\\Roaming\\Spotify\\Spotify.exe';
    const spotify = addProc({ name: 'Spotify.exe', parent: explorer.pid, exe: spotifyExe, cmd: `"${spotifyExe}" --autostart --minimized`, cpu: 0.7, mem: 132, title: 'Spotify Premium', start: at(56) });
    [['--type=gpu-process', 0.2, 70, 0.6], ['--type=renderer', 0.6, 160, 0]]
      .forEach(([args, cpu, mem, gpu]) => addProc({ name: 'Spotify.exe', parent: spotify.pid, exe: spotifyExe, cmd: `"${spotifyExe}" ${args}`, cpu, mem, gpu, start: at(57) }));

    const notionExe = 'C:\\Users\\Hesh\\AppData\\Local\\Programs\\Notion\\Notion.exe';
    const notion = addProc({ name: 'Notion.exe', parent: explorer.pid, exe: notionExe, cmd: `"${notionExe}"`, cpu: 0.3, mem: 104, title: 'HOME', start: at(4100) });
    [['--type=gpu-process', 0.1, 64, 0.3], ['--type=renderer', 0.4, 248, 0]]
      .forEach(([args, cpu, mem, gpu]) => addProc({ name: 'Notion.exe', parent: notion.pid, exe: notionExe, cmd: `"${notionExe}" ${args}`, cpu, mem, gpu, start: at(4101) }));

    addProc({ name: 'obs64.exe', parent: explorer.pid, exe: 'C:\\Program Files\\obs-studio\\bin\\64bit\\obs64.exe', cmd: '"C:\\Program Files\\obs-studio\\bin\\64bit\\obs64.exe"', cpu: 3.8, mem: 412, gpu: 11, title: 'OBS 30.2.3 - Profile: Stream - Scenes: Main', start: at(7000), io: 1.5, threads: 61, priority: 'aboveNormal', cwd: 'C:\\Program Files\\obs-studio\\bin\\64bit' });
    addProc({ name: 'Adobe Premiere Pro.exe', parent: explorer.pid, exe: 'C:\\Program Files\\Adobe\\Adobe Premiere Pro 2025\\Adobe Premiere Pro.exe', cmd: '"C:\\Program Files\\Adobe\\Adobe Premiere Pro 2025\\Adobe Premiere Pro.exe"', cpu: 12.4, mem: 2460, gpu: 1.2, title: 'Adobe Premiere Pro 2025 - E:\\Work\\Edits\\client-reel.prproj *', hung: true, start: at(6600), io: 2, threads: 142, handles: 5200 });
    const ccExe = 'C:\\Program Files (x86)\\Common Files\\Adobe\\Adobe Desktop Common\\ADS\\Adobe Desktop Service.exe';
    addProc({ name: 'Adobe Desktop Service.exe', parent: explorer.pid, exe: ccExe, cmd: `"${ccExe}" --mode=0`, cpu: 0.2, mem: 74, start: at(60) });
    addProc({ name: 'CCXProcess.exe', parent: explorer.pid, exe: 'C:\\Program Files\\Adobe\\Adobe Creative Cloud Experience\\CCXProcess.exe', cmd: '"C:\\Program Files\\Adobe\\Adobe Creative Cloud Experience\\CCXProcess.exe"', cpu: 0, mem: 12, start: at(62) });
    addProc({ name: 'AdobeIPCBroker.exe', parent: explorer.pid, exe: 'C:\\Program Files (x86)\\Common Files\\Adobe\\OOBE\\PDApp\\IPC\\AdobeIPCBroker.exe', cmd: '"C:\\Program Files (x86)\\Common Files\\Adobe\\OOBE\\PDApp\\IPC\\AdobeIPCBroker.exe" -launchedbyvulcan', cpu: 0, mem: 11, start: at(63) });

    const steamExe = 'C:\\Program Files (x86)\\Steam\\steam.exe';
    const steam = addProc({ name: 'steam.exe', parent: explorer.pid, exe: steamExe, cmd: `"${steamExe}" -silent`, cpu: 0.3, mem: 96, start: at(58) });
    const webhelper = 'C:\\Program Files (x86)\\Steam\\bin\\cef\\cef.win7x64\\steamwebhelper.exe';
    for (let i = 0; i < 3; i++) addProc({ name: 'steamwebhelper.exe', parent: steam.pid, exe: webhelper, cmd: `"${webhelper}" --type=${['gpu-process', 'utility', 'renderer'][i]} -lang=en_US`, cpu: 0.1, mem: 40 + i * 30, gpu: i === 0 ? 0.2 : 0, start: at(61) });

    const termExe = 'C:\\Program Files\\WindowsApps\\Microsoft.WindowsTerminal_1.21.2911.0_x64__8wekyb3d8bbwe\\WindowsTerminal.exe';
    const term = addProc({ name: 'WindowsTerminal.exe', parent: explorer.pid, exe: termExe, cmd: `"${termExe}"`, cpu: 0.2, mem: 88, gpu: 0.3, title: 'Windows PowerShell', start: at(6050) });
    addProc({ name: 'OpenConsole.exe', parent: term.pid, exe: termExe.replace('WindowsTerminal.exe', 'OpenConsole.exe'), cmd: '--headless --width 120 --height 30 --signal 0x9d4 --server 0x9cc', cpu: 0, mem: 7, start: at(6051) });
    const ps = addProc({ name: 'powershell.exe', parent: term.pid, exe: `${SYS32}\\WindowsPowerShell\\v1.0\\powershell.exe`, cmd: `"${SYS32}\\WindowsPowerShell\\v1.0\\powershell.exe" -NoLogo`, cpu: 0, mem: 74, start: at(6052), cwd: 'E:\\Work\\LegacyHub\\legacyhubmain' });
    addProc({ name: 'node.exe', parent: ps.pid, exe: 'C:\\Program Files\\nodejs\\node.exe', cmd: '"C:\\Program Files\\nodejs\\node.exe" server.js --port 5173', cpu: 0.4, mem: 92, start: at(6200), cwd: 'E:\\Work\\LegacyHub\\legacyhubmain' });
    addProc({ name: 'node.exe', parent: ps.pid, exe: 'C:\\Program Files\\nodejs\\node.exe', cmd: '"C:\\Program Files\\nodejs\\node.exe" node_modules\\vite\\bin\\vite.js build --watch', cpu: 0.9, mem: 136, start: at(6210), io: 0.3, cwd: 'E:\\Work\\LegacyHub\\apps\\pitchkit' });
    for (let i = 0; i < 2; i++) addProc({ name: 'conhost.exe', parent: i ? ps.pid : svcPids.Schedule, exe: `${SYS32}\\conhost.exe`, cmd: `\\??\\C:\\Windows\\system32\\conhost.exe 0x4`, cpu: 0, mem: 6 });

    const orbitExe = 'E:\\Work\\LegacyHub\\apps\\orbit\\src-tauri\\target\\release\\orbit.exe';
    const orbit = addProc({ name: 'orbit.exe', parent: explorer.pid, exe: orbitExe, cmd: `"${orbitExe}"`, cpu: 0.6, mem: 38, title: 'Orbit', start: Math.floor(now() / 1000) - 340 });
    state.ownPid = orbit.pid;
    const wv = 'C:\\Program Files (x86)\\Microsoft\\EdgeWebView\\Application\\130.0.2849.80\\msedgewebview2.exe';
    ['--embedded-browser-webview=1 --webview-exe-name=orbit.exe', '--type=gpu-process', '--type=utility --utility-sub-type=network.mojom.NetworkService', '--type=utility --utility-sub-type=storage.mojom.StorageService', '--type=renderer', '--type=crashpad-handler']
      .forEach((args, i) => addProc({ name: 'msedgewebview2.exe', parent: orbit.pid, exe: wv, cmd: `"${wv}" ${args}`, cpu: [0.2, 0.4, 0.05, 0, 0.9, 0][i], mem: [64, 98, 24, 16, 132, 7][i], gpu: i === 1 ? 0.8 : 0, start: orbit.startTime + 1 }));

    state.svcPids = svcPids;
  }

  // ------------------------------------------------------------ services + startup

  function seedServices() {
    const sp = state.svcPids;
    const list = [
      ['AudioSrv', 'Windows Audio', 'running', 'auto', 'Manages audio for Windows-based programs.'],
      ['AudioEndpointBuilder', 'Windows Audio Endpoint Builder', 'running', 'auto', 'Manages audio devices for the Windows Audio service.'],
      ['BITS', 'Background Intelligent Transfer Service', 'running', 'autoDelayed', 'Transfers files in the background using idle network bandwidth.'],
      ['BthServ', 'Bluetooth Support Service', 'stopped', 'manual', 'Supports discovery and association of remote Bluetooth devices.'],
      ['CryptSvc', 'Cryptographic Services', 'running', 'auto', 'Provides catalog, root and key management services.'],
      ['DcomLaunch', 'DCOM Server Process Launcher', 'running', 'auto', 'Launches COM and DCOM servers in response to object activation requests.'],
      ['Dhcp', 'DHCP Client', 'running', 'auto', 'Registers and updates IP addresses and DNS records for this computer.'],
      ['Dnscache', 'DNS Client', 'running', 'auto', 'Caches Domain Name System (DNS) names and registers the full computer name.'],
      ['DiagTrack', 'Connected User Experiences and Telemetry', 'running', 'auto', null],
      ['EventLog', 'Windows Event Log', 'running', 'auto', 'Manages events and event logs.'],
      ['Fax', 'Fax', 'stopped', 'manual', 'Enables you to send and receive faxes.'],
      ['iphlpsvc', 'IP Helper', 'running', 'auto', 'Provides tunnel connectivity using IPv6 transition technologies.'],
      ['LanmanServer', 'Server', 'running', 'auto', 'Supports file, print and named-pipe sharing over the network.'],
      ['LanmanWorkstation', 'Workstation', 'running', 'auto', 'Creates and maintains client network connections to remote servers.'],
      ['lmhosts', 'TCP/IP NetBIOS Helper', 'running', 'manual', 'Provides support for the NetBIOS over TCP/IP service.'],
      ['MpsSvc', 'Windows Defender Firewall', 'running', 'auto', 'Helps protect your computer by preventing unauthorized access.'],
      ['nsi', 'Network Store Interface Service', 'running', 'auto', 'Delivers network notifications to user mode clients.'],
      ['NVDisplay.ContainerLocalSystem', 'NVIDIA Display Container LS', 'running', 'auto', 'Container service for NVIDIA root features.'],
      ['PcaSvc', 'Program Compatibility Assistant Service', 'running', 'manual', 'Provides support for the Program Compatibility Assistant.'],
      ['ProfSvc', 'User Profile Service', 'running', 'auto', 'Responsible for loading and unloading user profiles.'],
      ['RemoteRegistry', 'Remote Registry', 'stopped', 'disabled', 'Enables remote users to modify registry settings on this computer.'],
      ['RpcSs', 'Remote Procedure Call (RPC)', 'running', 'auto', 'The service control manager for COM and DCOM servers.'],
      ['RtkAudioUniversalService', 'Realtek Audio Universal Service', 'running', 'auto', null],
      ['Schedule', 'Task Scheduler', 'running', 'auto', 'Enables a user to configure and schedule automated tasks.'],
      ['SecurityHealthService', 'Windows Security Service', 'running', 'manual', 'Handles unified device protection and health information.'],
      ['seclogon', 'Secondary Logon', 'stopped', 'manual', 'Enables starting processes under alternate credentials.'],
      ['Spooler', 'Print Spooler', 'running', 'auto', 'Spools print jobs and handles interaction with the printer.'],
      ['SSDPSRV', 'SSDP Discovery', 'stopped', 'manual', 'Discovers networked devices and services that use the SSDP protocol.'],
      ['Steam Client Service', 'Steam Client Service', 'running', 'manual', 'Steam Client Service monitors and updates Steam content.'],
      ['SysMain', 'SysMain', 'running', 'auto', 'Maintains and improves system performance over time.'],
      ['TermService', 'Remote Desktop Services', 'stopped', 'manual', 'Allows users to connect interactively to a remote computer.'],
      ['Themes', 'Themes', 'running', 'auto', 'Provides user experience theme management.'],
      ['TrkWks', 'Distributed Link Tracking Client', 'running', 'auto', 'Maintains links between NTFS files within a computer or across computers.'],
      ['UserManager', 'User Manager', 'running', 'auto', 'Provides the runtime components required for multi-user interaction.'],
      ['UsoSvc', 'Update Orchestrator Service', 'running', 'autoDelayed', 'Manages Windows Updates.'],
      ['W32Time', 'Windows Time', 'running', 'manual', 'Maintains date and time synchronization on all clients and servers.'],
      ['WdNisSvc', 'Microsoft Defender Antivirus Network Inspection Service', 'running', 'manual', 'Helps guard against intrusion attempts targeting known vulnerabilities.'],
      ['WinDefend', 'Microsoft Defender Antivirus Service', 'running', 'auto', 'Helps protect users from malware and other potentially unwanted software.'],
      ['WinHttpAutoProxySvc', 'WinHTTP Web Proxy Auto-Discovery Service', 'running', 'manual', null],
      ['Winmgmt', 'Windows Management Instrumentation', 'running', 'auto', 'Provides a common interface and object model to access management information.'],
      ['WlanSvc', 'WLAN AutoConfig', 'running', 'auto', 'Provides the logic required to configure, discover, connect to and disconnect from a WLAN.'],
      ['WSearch', 'Windows Search', 'running', 'autoDelayed', 'Provides content indexing, property caching and search results.'],
      ['wuauserv', 'Windows Update', 'running', 'manual', 'Enables the detection, download and installation of updates for Windows.'],
      ['XblAuthManager', 'Xbox Live Auth Manager', 'stopped', 'manual', 'Provides authentication and authorization services for Xbox Live.'],
      ['XboxNetApiSvc', 'Xbox Live Networking Service', 'stopped', 'manual', null],
    ];
    state.services = list.map(([name, displayName, status, startType, description]) => ({
      name, displayName, status, startType, description,
      pid: status === 'running' ? (sp[name] ?? sp.DcomLaunch ?? null) : null,
    }));
    for (const s of state.services) {
      if (s.status === 'running' && !sp[s.name]) {
        // Shared hosts: point at a svchost that exists.
        const hosts = [...state.procs.values()].filter((p) => p.name === 'svchost.exe');
        s.pid = hosts[(s.name.length * 7) % hosts.length].pid;
      }
    }
  }

  function seedStartup() {
    state.startup = [
      { id: 'HKCU\\Run\\Discord', name: 'Discord', command: '"C:\\Users\\Hesh\\AppData\\Local\\Discord\\Update.exe" --processStart Discord.exe', location: 'HKCU Run', enabled: true, path: 'C:\\Users\\Hesh\\AppData\\Local\\Discord\\Update.exe', needsAdmin: false },
      { id: 'HKCU\\Run\\Spotify', name: 'Spotify', command: '"C:\\Users\\Hesh\\AppData\\Roaming\\Spotify\\Spotify.exe" --autostart --minimized', location: 'HKCU Run', enabled: true, path: 'C:\\Users\\Hesh\\AppData\\Roaming\\Spotify\\Spotify.exe', needsAdmin: false },
      { id: 'HKCU\\Run\\Steam', name: 'Steam', command: '"C:\\Program Files (x86)\\Steam\\steam.exe" -silent', location: 'HKCU Run', enabled: false, path: 'C:\\Program Files (x86)\\Steam\\steam.exe', needsAdmin: false },
      { id: 'HKCU\\Run\\OneDrive', name: 'OneDrive', command: '"C:\\Users\\Hesh\\AppData\\Local\\Microsoft\\OneDrive\\OneDrive.exe" /background', location: 'HKCU Run', enabled: true, path: 'C:\\Users\\Hesh\\AppData\\Local\\Microsoft\\OneDrive\\OneDrive.exe', needsAdmin: false },
      { id: 'HKCU\\Run\\Notion', name: 'Notion', command: '"C:\\Users\\Hesh\\AppData\\Local\\Programs\\Notion\\Notion.exe" --startup', location: 'HKCU Run', enabled: false, path: 'C:\\Users\\Hesh\\AppData\\Local\\Programs\\Notion\\Notion.exe', needsAdmin: false },
      { id: 'HKLM\\Run\\SecurityHealth', name: 'SecurityHealth', command: '%windir%\\system32\\SecurityHealthSystray.exe', location: 'HKLM Run', enabled: true, path: 'C:\\Windows\\system32\\SecurityHealthSystray.exe', needsAdmin: true },
      { id: 'HKLM\\Run\\RtkAudUService', name: 'RtkAudUService', command: '"C:\\Windows\\System32\\DriverStore\\FileRepository\\realtekservice.inf_amd64_8c2b1a0f9e7d6c51\\RtkAudUService64.exe" -background', location: 'HKLM Run', enabled: true, path: 'C:\\Windows\\System32\\DriverStore\\FileRepository\\realtekservice.inf_amd64_8c2b1a0f9e7d6c51\\RtkAudUService64.exe', needsAdmin: true },
      { id: 'HKLM\\Run32\\Adobe CCXProcess', name: 'Adobe CCXProcess', command: '"C:\\Program Files\\Adobe\\Adobe Creative Cloud Experience\\CCXProcess.exe"', location: 'HKLM Run (32-bit)', enabled: true, path: 'C:\\Program Files\\Adobe\\Adobe Creative Cloud Experience\\CCXProcess.exe', needsAdmin: true },
      { id: 'HKLM\\Run32\\Adobe Creative Cloud', name: 'Adobe Creative Cloud', command: '"C:\\Program Files\\Adobe\\Adobe Creative Cloud\\ACC\\Creative Cloud.exe" --showwindow=false --onOSstartup=true', location: 'HKLM Run (32-bit)', enabled: false, path: 'C:\\Program Files\\Adobe\\Adobe Creative Cloud\\ACC\\Creative Cloud.exe', needsAdmin: true },
      { id: 'UserFolder\\OBS Studio.lnk', name: 'OBS Studio', command: 'C:\\Program Files\\obs-studio\\bin\\64bit\\obs64.exe', location: 'Startup folder', enabled: true, path: 'C:\\Program Files\\obs-studio\\bin\\64bit\\obs64.exe', needsAdmin: false },
      { id: 'UserFolder\\Orbit.lnk', name: 'Orbit', command: 'E:\\Work\\LegacyHub\\apps\\orbit\\src-tauri\\target\\release\\orbit.exe', location: 'Startup folder', enabled: false, path: 'E:\\Work\\LegacyHub\\apps\\orbit\\src-tauri\\target\\release\\orbit.exe', needsAdmin: false },
      { id: 'CommonFolder\\Send to OneNote.lnk', name: 'Send to OneNote', command: 'C:\\ProgramData\\Microsoft\\Windows\\Start Menu\\Programs\\StartUp\\Send to OneNote.lnk', location: 'Startup folder (all users)', enabled: true, path: null, needsAdmin: true },
    ];
    // Sample startup impact, as if Orbit measured the last sign-in (impact.rs).
    const measuredAt = Math.floor(now() / 1000) - 26 * 3600;
    const impacts = { Discord: 'high', Spotify: 'medium', OneDrive: 'medium', SecurityHealth: 'low', RtkAudUService: 'low', 'Adobe CCXProcess': 'high', 'OBS Studio': 'medium' };
    for (const e of state.startup) {
      e.impact = impacts[e.name] || null;
      e.impactMeasuredAt = e.impact ? measuredAt : null;
    }
    state.measureScheduled = false;
  }

  seedProcesses();
  seedServices();
  seedStartup();

  // ------------------------------------------------------------ sampling

  function driftProc(p, secs) {
    if (p.suspended) {
      p.cpu = 0;
      p.diskReadBps = 0;
      p.diskWriteBps = 0;
      if (!NO_GPU) p.gpu = 0;
      return;
    }
    if (p.notResponding) {
      // A stuck UI thread pinning one logical core.
      p.cpu = clamp(100 / CORES - 0.3 + noise() * 0.4, 0, 100);
    } else {
      const base = p.cpuBase;
      let c = p.cpu + (base - p.cpu) * 0.3 + noise() * Math.max(0.03, base * 0.45);
      if (Math.random() < 0.004 * secs && base > 0.1) c += 6 + Math.random() * 22;
      p.cpu = clamp(c, 0, 100);
    }
    p.memory = Math.max(256 * 1024, p.memory + (p.memBase - p.memory) * 0.05 + noise() * p.memBase * 0.012);
    if (p.ioBias > 0 && Math.random() < 0.35) {
      p.diskReadBps = Math.random() * p.ioBias * 6 * MB;
      p.diskWriteBps = Math.random() < 0.5 ? Math.random() * p.ioBias * 2.5 * MB : 0;
    } else {
      p.diskReadBps *= 0.4;
      p.diskWriteBps *= 0.3;
      if (p.diskReadBps < 2048) p.diskReadBps = 0;
      if (p.diskWriteBps < 2048) p.diskWriteBps = 0;
    }
    p.totalRead += p.diskReadBps * secs;
    p.totalWritten += p.diskWriteBps * secs;
    if (!NO_GPU) p.gpu = p.gpuBase > 0 ? clamp(p.gpuBase + noise() * p.gpuBase * 0.35, 0, 100) : 0;
    p.threads = Math.max(1, p.threads + (Math.random() < 0.05 ? (Math.random() < 0.5 ? -1 : 1) : 0));
  }

  function rowOf(p) {
    const row = {
      pid: p.pid,
      parentPid: p.parentPid,
      name: p.name,
      exe: p.exe,
      cmd: p.cmd,
      user: p.user,
      cpu: Math.round(p.cpu * 100) / 100,
      memory: Math.round(p.memory),
      diskReadBps: Math.round(p.diskReadBps),
      diskWriteBps: Math.round(p.diskWriteBps),
      gpu: p.gpu === null ? null : Math.round(p.gpu * 10) / 10,
      status: p.status,
      startTime: p.startTime,
      kind: p.kind,
      windowTitle: p.windowTitle,
      notResponding: p.notResponding,
      suspended: p.suspended,
      sessionId: null,
      netRxBps: null,
      netTxBps: null,
    };
    return mockRegistry.decorateRow(row, p);
  }

  function sample() {
    const secs = state.intervalMs / 1000;
    state.tick++;

    // A short-lived process now and then, like a search indexer worker.
    const nowS = Math.floor(now() / 1000);
    if (state.transientPid === null && state.tick % 25 === 0) {
      const p = addProc({ name: 'SearchProtocolHost.exe', parent: state.svcPids.WSearch, exe: `${SYS32}\\SearchProtocolHost.exe`, cmd: `"${SYS32}\\SearchProtocolHost.exe" Global\\UsGthrFltPipeMssGthrPipe12_ Global\\UsGthrCtrlFltPipeMssGthrPipe12 1 -2147483646 "Software\\Microsoft\\Windows Search" "Mozilla/4.0 (compatible; MSIE 6.0; Windows NT; MS Search 4.0 Robot)" "C:\\ProgramData\\Microsoft\\Search\\Data\\Temp\\usgthrsvc" "DownLevelDaemon"`, user: 'SYSTEM', cpu: 1.8, mem: 9, io: 1, start: nowS });
      state.transientPid = p.pid;
      state.transientUntil = state.tick + 8;
    } else if (state.transientPid !== null && state.tick >= state.transientUntil) {
      state.procs.delete(state.transientPid);
      usedPids.delete(state.transientPid);
      state.transientPid = null;
    }

    let cpuSum = 0;
    let memSum = 0;
    let readSum = 0;
    let writeSum = 0;
    let gpuSum = 0;
    const rows = [];
    for (const p of state.procs.values()) {
      driftProc(p, secs);
      cpuSum += p.cpu;
      memSum += p.memory;
      readSum += p.diskReadBps;
      writeSum += p.diskWriteBps;
      gpuSum += p.gpu || 0;
      rows.push(rowOf(p));
    }

    const usage = clamp(cpuSum + 1.2 + Math.random() * 0.8, 0, 100);
    const perCore = [];
    let coreSum = 0;
    for (let i = 0; i < CORES; i++) {
      const v = clamp(usage * (0.55 + Math.random() * 0.9) + (i === 3 ? 6 : 0), 0, 100);
      perCore.push(v);
      coreSum += v;
    }
    const scale = coreSum > 0 ? (usage * CORES) / coreSum : 1;
    for (let i = 0; i < CORES; i++) perCore[i] = Math.round(clamp(perCore[i] * scale, 0, 100) * 10) / 10;

    state.freq = clamp(state.freq + noise() * 60 + (usage > 30 ? 40 : -20), 3800, 4500);
    const used = clamp(memSum + 1.65 * GB, 0, TOTAL_MEM * 0.94);
    state.cached = clamp(state.cached + noise() * 30 * MB, 2.6 * GB, 4.2 * GB);
    state.swapUsed = clamp(state.swapUsed + noise() * 6 * MB, 0.4 * GB, 0.9 * GB);
    state.threads = Math.round(clamp(state.threads + noise() * 12, 3200, 3700));
    state.handles = Math.round(clamp(state.handles + noise() * 220, 141000, 156000));

    const disk = {
      activePct: Math.round(clamp(((readSum + writeSum) / (180 * MB)) * 100 + Math.random() * 1.5, 0, 100) * 10) / 10,
      readBps: Math.round(readSum + Math.random() * 40 * 1024),
      writeBps: Math.round(writeSum + Math.random() * 120 * 1024),
    };

    const interfaces = [];
    let rx = 0;
    let tx = 0;
    for (const name of Object.keys(state.netTotals)) {
      const cur = state.netCur[name];
      let r = 0;
      let t = 0;
      if (name === 'Ethernet') {
        const burst = Math.random() < 0.15;
        r = clamp(cur[0] * 0.6 + (burst ? Math.random() * 4.5 * MB : Math.random() * 180 * 1024), 0, 60 * MB);
        t = clamp(cur[1] * 0.6 + (Math.random() < 0.1 ? Math.random() * 900 * 1024 : Math.random() * 40 * 1024), 0, 20 * MB);
      } else if (name === 'vEthernet (WSL)') {
        r = Math.random() < 0.2 ? Math.random() * 30 * 1024 : 0;
        t = Math.random() < 0.2 ? Math.random() * 8 * 1024 : 0;
      }
      cur[0] = r;
      cur[1] = t;
      state.netTotals[name][0] += r * secs;
      state.netTotals[name][1] += t * secs;
      rx += r;
      tx += t;
      interfaces.push({
        name,
        rxBps: Math.round(r),
        txBps: Math.round(t),
        totalRx: Math.round(state.netTotals[name][0]),
        totalTx: Math.round(state.netTotals[name][1]),
      });
    }

    const snap = {
      ts: now(),
      intervalMs: state.intervalMs,
      cpu: { usage: Math.round(usage * 10) / 10, perCore, freqMhz: Math.round(state.freq) },
      memory: {
        total: TOTAL_MEM,
        used: Math.round(used),
        available: Math.round(TOTAL_MEM - used),
        swapTotal: Math.round(4.75 * GB),
        swapUsed: Math.round(state.swapUsed),
        committed: Math.round(used + 3.1 * GB),
        commitLimit: Math.round(TOTAL_MEM + 4.75 * GB),
        cached: Math.round(state.cached),
        // Free + zeroed pages: what is available beyond the standby part of cached.
        free: Math.round(clamp(TOTAL_MEM - used - state.cached + 0.22 * GB, 0, TOTAL_MEM - used)),
      },
      disk,
      network: { rxBps: Math.round(rx), txBps: Math.round(tx), interfaces, perApp: { available: false, reason: null } },
      gpu: NO_GPU
        ? { usage: null, dedicatedUsed: null }
        : { usage: Math.round(clamp(gpuSum + 1.5 + Math.random(), 0, 100) * 10) / 10, dedicatedUsed: Math.round(2.35 * GB + Math.random() * 40 * MB) },
      system: {
        uptimeSecs: Math.floor(now() / 1000) - BOOT,
        processCount: rows.length,
        threadCount: state.threads,
        handleCount: state.handles,
      },
      disks: [],
      gpuEngines: [],
      battery: null,
      processes: rows,
    };
    mockRegistry.decorateSnapshot(snap);
    state.last = snap;
    return snap;
  }

  // SnapshotDelta: the same event the Rust sampler sends in delta mode (docs/CONTRACT.md).
  function buildDelta(snap) {
    const current = new Map(snap.processes.map((r) => [r.pid, r.startTime]));
    const removed = [];
    for (const [pid, start] of state.emitted) if (current.get(pid) !== start) removed.push(pid);
    removed.sort((a, b) => a - b);
    const { processes, ...stats } = snap;
    return {
      ...stats,
      added: processes.filter((r) => state.emitted.get(r.pid) !== r.startTime),
      removed,
      rows: processes.map((r) => ({
        pid: r.pid, cpu: r.cpu, memory: r.memory, diskReadBps: r.diskReadBps, diskWriteBps: r.diskWriteBps,
        gpu: r.gpu, status: r.status, kind: r.kind, windowTitle: r.windowTitle,
        notResponding: r.notResponding, suspended: r.suspended,
        sessionId: r.sessionId, netRxBps: r.netRxBps, netTxBps: r.netTxBps,
      })),
    };
  }

  function emitSample() {
    const snap = sample();
    let payload = snap;
    if (state.delta) {
      if (!state.deltaMode) {
        // Just switched on: the first delta carries every row.
        state.emitted.clear();
        state.deltaMode = true;
      }
      payload = buildDelta(snap);
    } else {
      state.deltaMode = false;
    }
    state.emitted = new Map(snap.processes.map((r) => [r.pid, r.startTime]));
    for (const cb of [...state.listeners]) {
      try {
        cb(JSON.parse(JSON.stringify(payload)));
      } catch (err) {
        console.error('[orbit mock] snapshot listener failed', err);
      }
    }
  }

  function restartTimer() {
    clearInterval(state.timer);
    state.timer = 0;
    if (!state.paused && state.listeners.size) {
      state.timer = setInterval(emitSample, state.intervalMs);
    }
  }

  function subscribe(cb) {
    state.listeners.add(cb);
    if (!state.timer) restartTimer();
    return () => {
      state.listeners.delete(cb);
      if (!state.listeners.size) restartTimer();
    };
  }

  // ------------------------------------------------------------ command helpers

  const DENIED = 'Access denied. Try Restart as administrator.';
  const PROTECTED_MSG = 'Access denied. Windows protects this process.';

  function findProc(pid) {
    const p = state.procs.get(Number(pid));
    if (!p) throw 'Process not found. It may have already exited.';
    return p;
  }

  // Process actions pass the row's startTime; 0 or omitted skips the check (as in Rust).
  function checkStart(p, startTime) {
    const t = Number(startTime);
    if (Number.isFinite(t) && t > 0 && Math.abs(t - p.startTime) > 1) throw 'That process has exited.';
  }

  // As in Rust (WINDOWS_CRITICAL, DESKTOP_CRITICAL): ending or suspending these takes Windows down.
  const WINDOWS_NEEDS_IT = 'Windows needs this process. Ending it would crash the PC.';
  const CRITICAL = new Set([
    'system', 'registry', 'smss.exe', 'csrss.exe', 'wininit.exe', 'services.exe', 'lsass.exe',
    'winlogon.exe', 'memory compression', 'secure system',
  ]);
  const DESKTOP = new Set([
    'dwm.exe', 'winlogon.exe', 'csrss.exe', 'wininit.exe', 'smss.exe', 'services.exe', 'lsass.exe', 'fontdrvhost.exe',
  ]);

  // verb: 'end' (End task, End tree) or 'suspend'; other actions pass none.
  function guard(p, verb) {
    if (verb && p.pid === 0) throw `The System Idle Process only counts unused CPU time, so it can't be ${verb}ed.`;
    if (verb && p.pid === 4) throw WINDOWS_NEEDS_IT;
    if (p.pid === state.ownPid) throw "Orbit can't do that to itself.";
    const n = p.name.toLowerCase();
    if (verb === 'end' && CRITICAL.has(n)) throw WINDOWS_NEEDS_IT;
    if (verb === 'suspend' && DESKTOP.has(n)) throw `${p.name} is needed to draw or run the desktop; suspending it would freeze Windows with no way to resume it.`;
    if (verb === 'suspend' && CRITICAL.has(n)) throw `${p.name} is a critical Windows process; suspending it would freeze Windows with no way to resume it.`;
    if (PROTECTED.has(n)) throw PROTECTED_MSG;
    if (!state.isAdmin && p.user !== 'Hesh') throw DENIED;
  }

  function removeProc(pid) {
    state.procs.delete(pid);
    for (const s of state.services) if (s.pid === pid) { s.pid = null; s.status = 'stopped'; }
  }

  function descendants(pid) {
    const out = [];
    const walk = (parent) => {
      for (const p of state.procs.values()) {
        if (p.parentPid === parent && p.pid !== parent && !out.includes(p)) {
          out.push(p);
          walk(p.pid);
        }
      }
    };
    walk(pid);
    return out;
  }

  const PRIORITIES = ['idle', 'belowNormal', 'normal', 'aboveNormal', 'high', 'realtime'];
  const clone = (v) => (v === undefined ? null : JSON.parse(JSON.stringify(v)));
  const delay = (ms) => new Promise((r) => setTimeout(r, ms));

  const handlers = {
    get_snapshot() {
      return state.last || sample();
    },

    get_static_info() {
      return {
        cpuBrand: 'Intel(R) Core(TM) i7-7700K CPU @ 4.20GHz',
        logicalCores: CORES,
        physicalCores: PHYSICAL,
        totalMemory: TOTAL_MEM,
        hostName: 'LEGACY-PC',
        osName: 'Windows 10 Pro',
        osVersion: '10.0.19045',
        kernelVersion: '19045',
        isAdmin: state.isAdmin,
        pid: state.ownPid,
        bootTime: BOOT,
        disks: [
          { name: 'Local Disk', mount: 'C:\\', fileSystem: 'NTFS', total: 511101108224, available: 183492534272, kind: 'SSD', removable: false },
          { name: 'Data', mount: 'D:\\', fileSystem: 'NTFS', total: 2000396742656, available: 912814473216, kind: 'HDD', removable: false },
          { name: 'Work', mount: 'E:\\', fileSystem: 'NTFS', total: 1000186310656, available: 388124442624, kind: 'SSD', removable: false },
        ],
      };
    },

    set_sampling({ intervalMs, paused, delta }) {
      const iv = Number(intervalMs);
      if (Number.isFinite(iv) && iv >= 250) state.intervalMs = Math.round(iv);
      state.paused = !!paused;
      if (typeof delta === 'boolean') state.delta = delta;
      restartTimer();
      return null;
    },

    end_task({ pid, startTime }) {
      const p = findProc(pid);
      checkStart(p, startTime);
      guard(p, 'end');
      removeProc(p.pid);
      return null;
    },

    end_tree({ pid, startTime }) {
      const p = findProc(pid);
      checkStart(p, startTime);
      guard(p, 'end');
      const all = [p, ...descendants(p.pid)];
      let n = 0;
      for (const q of all) {
        if (PROTECTED.has(q.name.toLowerCase()) || (!state.isAdmin && q.user !== 'Hesh')) continue;
        removeProc(q.pid);
        n++;
      }
      return n;
    },

    suspend_process({ pid, startTime }) {
      const p = findProc(pid);
      checkStart(p, startTime);
      guard(p, 'suspend');
      p.suspended = true;
      return null;
    },

    resume_process({ pid, startTime }) {
      const p = findProc(pid);
      checkStart(p, startTime);
      guard(p);
      p.suspended = false;
      return null;
    },

    get_priority({ pid }) {
      const p = findProc(pid);
      if (PROTECTED.has(p.name.toLowerCase()) || (!state.isAdmin && p.user !== 'Hesh' && p.user !== 'SYSTEM')) throw DENIED;
      return p.priority;
    },

    set_priority({ pid, priority, startTime }) {
      const p = findProc(pid);
      if (!PRIORITIES.includes(priority)) throw 'Unknown priority.';
      checkStart(p, startTime);
      guard(p);
      // Without admin rights Windows quietly caps realtime at high.
      p.priority = priority === 'realtime' && !state.isAdmin ? 'high' : priority;
      return null;
    },

    get_process_details({ pid }) {
      const p = findProc(pid);
      const parent = p.parentPid !== null ? state.procs.get(p.parentPid) : null;
      const locked = PROTECTED.has(p.name.toLowerCase());
      const visible = state.isAdmin || p.user === 'Hesh';
      return {
        pid: p.pid,
        parentPid: p.parentPid,
        parentName: parent ? parent.name : null,
        name: p.name,
        exe: p.exe,
        cmd: p.cmd,
        cwd: visible && !locked ? p.cwd : null,
        user: p.user,
        startTime: p.startTime,
        priority: locked ? null : p.priority,
        threads: p.threads,
        handles: locked ? null : p.handles,
        memory: Math.round(p.memory),
        virtualMemory: Math.round(p.virt),
        children: [...state.procs.values()].filter((c) => c.parentPid === p.pid && c.pid !== p.pid).map((c) => ({ pid: c.pid, name: c.name })),
        diskTotalRead: Math.round(p.totalRead),
        diskTotalWritten: Math.round(p.totalWritten),
      };
    },

    open_location({ path }) {
      if (!path) throw 'No file location is known for this process.';
      return null;
    },

    search_online({ query }) {
      if (!String(query || '').trim()) throw 'Nothing to search for.';
      return null;
    },

    open_url({ url }) {
      if (!String(url || '').startsWith('https://')) throw 'Only https links can be opened.';
      return null;
    },

    restart_as_admin() {
      // In the browser preview there is nothing to relaunch: act as if it worked.
      state.isAdmin = true;
      return null;
    },

    export_report({ fileName, contents }) {
      const safe = String(fileName || 'orbit-report.csv').replace(/[\\/:*?"<>|\u0000-\u001f]/g, '_').trim() || 'orbit-report.csv';
      void contents;
      return `C:\\Users\\you\\Downloads\\${safe}`;
    },

    get_icons() {
      return {};
    },

    restart_process({ pid, startTime }) {
      const p = findProc(pid);
      checkStart(p, startTime);
      guard(p);
      if (!p.exe) throw "Orbit can't read this process's path. Try Restart as administrator.";
      if (/\\(windowsapps|systemapps)\\/i.test(p.exe)) throw "Store apps can't be restarted this way. Open it again from Start.";
      const q = addProc({
        name: p.name, parent: p.parentPid, exe: p.exe, cmd: p.cmd, user: p.user,
        cpu: p.cpuBase / 0.5, mem: p.memBase / MB, io: p.ioBias, gpu: p.gpuBase,
        title: p.windowTitle, start: Math.floor(now() / 1000), cwd: p.cwd, priority: 'normal',
      });
      removeProc(p.pid);
      return q.pid;
    },

    check_update() {
      // The preview pretends a newer release exists so the update UI can be seen.
      return { version: '0.4.1', currentVersion: MOCK_VERSION, notes: 'Preview release notes.', date: null };
    },

    install_update() {
      throw 'Updates install only in the desktop app.';
    },

    run_task({ command, asAdmin }) {
      const text = String(command || '').trim();
      if (!text) throw 'Type the name of a program, folder, document or website.';
      const m = text.match(/^"([^"]*)"\s*(.*)$/) || text.match(/^(\S+)\s*(.*)$/);
      const file = m[1];
      const known = {
        notepad: [`${SYS32}\\notepad.exe`, 'Untitled - Notepad'],
        cmd: [`${SYS32}\\cmd.exe`, `${asAdmin ? 'Administrator: ' : ''}C:\\Windows\\System32\\cmd.exe`],
        calc: [`${SYS32}\\calc.exe`, 'Calculator'],
        mspaint: [`${SYS32}\\mspaint.exe`, 'Untitled - Paint'],
        powershell: [`${SYS32}\\WindowsPowerShell\\v1.0\\powershell.exe`, 'Windows PowerShell'],
      };
      const key = file.toLowerCase().replace(/\.exe$/, '');
      if (known[key]) {
        const [exe, title] = known[key];
        addProc({ name: exe.slice(exe.lastIndexOf('\\') + 1), parent: state.ownPid, exe, cmd: m[2] ? `${file} ${m[2]}` : file, cpu: 0.05, mem: 14, title, start: Math.floor(now() / 1000), cwd: 'C:\\Users\\Hesh' });
        return null;
      }
      if (/^[a-z][a-z0-9+.-]+:/i.test(file) || /^[a-z]:\\/i.test(file) || /^www\./i.test(file)) return null;
      throw `Windows can't find '${file}'. Check the name and try again.`;
    },

    get_affinity({ pid }) {
      const p = findProc(pid);
      if (PROTECTED.has(p.name.toLowerCase()) || (!state.isAdmin && p.user !== 'Hesh' && p.user !== 'SYSTEM')) throw DENIED;
      const system = String(2 ** CORES - 1);
      return { processMask: p.affinity || system, systemMask: system, logicalCores: CORES };
    },

    set_affinity({ pid, mask, startTime }) {
      const p = findProc(pid);
      checkStart(p, startTime);
      guard(p);
      const m = Number(mask);
      if (!Number.isInteger(m) || m < 0) throw 'That processor selection is not valid.';
      if (m === 0) throw 'Pick at least one processor.';
      if (m >= 2 ** CORES) throw 'That selection includes processors this PC does not have.';
      p.affinity = String(m);
      return null;
    },

    get_startup_measure() {
      return state.measureScheduled;
    },

    schedule_startup_measure() {
      state.measureScheduled = true;
      return null;
    },

    cancel_startup_measure() {
      state.measureScheduled = false;
      return null;
    },

    list_startup() {
      return state.startup;
    },

    set_startup_enabled({ id, enabled }) {
      const e = state.startup.find((x) => x.id === id);
      if (!e) throw 'Startup entry not found.';
      if (e.needsAdmin && !state.isAdmin) throw DENIED;
      e.enabled = !!enabled;
      return null;
    },

    list_services() {
      return state.services;
    },

    async control_service({ name, action }) {
      const s = state.services.find((x) => x.name === name);
      if (!s) throw 'Service not found.';
      if (!['start', 'stop', 'restart'].includes(action)) throw 'Unknown service action.';
      if (action === 'start' && s.startType === 'disabled') throw 'The service is disabled. Change its start type first.';
      if (action === 'start' && s.status === 'running') throw 'The service is already running.';
      if (action === 'stop' && s.status === 'stopped') throw 'The service is not running.';
      const hostFor = () => {
        const hosts = [...state.procs.values()].filter((p) => p.name === 'svchost.exe');
        return hosts.length ? hosts[(s.name.length * 7) % hosts.length].pid : null;
      };
      if (action === 'stop') {
        s.status = 'stopPending';
        setTimeout(() => { s.status = 'stopped'; s.pid = null; }, 1400);
      } else if (action === 'start') {
        s.status = 'startPending';
        setTimeout(() => { s.status = 'running'; s.pid = hostFor(); }, 1400);
      } else {
        s.status = 'stopPending';
        setTimeout(() => { s.status = 'startPending'; s.pid = null; }, 900);
        setTimeout(() => { s.status = 'running'; s.pid = hostFor(); }, 2000);
      }
      // Rust waits for the final state before it returns.
      await delay(action === 'restart' ? 2050 : 1450);
      return null;
    },
  };

  // v0.4 feature mocks (js/mock/*.js); one with the same name replaces the built-in handler.
  Object.assign(handlers, mockRegistry.handlers);
  const ctx = {
    snapshot: () => state.last || sample(),
    isAdmin: () => state.isAdmin,
    delay,
  };

  async function mockCall(cmd, args) {
    const h = handlers[cmd];
    await delay(cmd === 'get_snapshot' ? 5 : 25 + Math.random() * 70);
    if (!h) throw `Unknown command: ${cmd}`;
    try {
      return clone(await h(args, ctx));
    } catch (err) {
      throw typeof err === 'string' ? err : (err && err.message) || 'Something went wrong.';
    }
  }

  return { call: mockCall, subscribe };
})();
