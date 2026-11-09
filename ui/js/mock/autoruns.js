// Orbit / mock/autoruns: scheduled tasks, services, logon keys and image hijacks for the
// browser preview (autoruns feature, docs/V04-CONTRACT.md).
//
// Shaped like the real list: tasks only when a trigger runs them at log on or system
// start, services only when they start on their own and are not signed by Microsoft.
// The logon Shell value carries one extra program (a warning example).
// URL flags: ?hijacks adds two image hijacks (empty by default), ?logonok shows the
// Windows defaults, ?autorunsfail makes the command fail.

const SYS32 = 'C:\\Windows\\System32';
const params = typeof location !== 'undefined' ? new URLSearchParams(location.search) : new URLSearchParams();

export default {
  async list_autoruns(_args, ctx) {
    await ctx.delay(500);
    if (params.has('autorunsfail')) throw 'Could not open Task Scheduler (Access is denied.).';
    return {
      tasks: [
        {
          name: '\\Microsoft\\Windows\\Workplace Join\\Recovery-Check', path: `${SYS32}\\dsregcmd.exe`, author: 'Microsoft Corporation',
          enabled: true, triggers: ['At log on', 'On lock'], command: '%SystemRoot%\\System32\\dsregcmd.exe /checkrecovery',
          publisher: 'Microsoft Windows', signatureStatus: 'signed',
        },
        {
          name: '\\Microsoft\\Windows\\Windows Error Reporting\\QueueReporting', path: `${SYS32}\\wermgr.exe`, author: 'Microsoft Corporation',
          enabled: true, triggers: ['At system start', 'Custom trigger', 'One time at 02:00'], command: '%windir%\\system32\\wermgr.exe -upload',
          publisher: 'Microsoft Windows', signatureStatus: 'signed',
        },
        {
          name: '\\Microsoft\\Windows\\TextServicesFramework\\MsCtfMonitor', path: '', author: 'Microsoft Corporation',
          enabled: true, triggers: ['At log on'], command: 'COM handler {01575CFE-9A55-4003-A5E1-F38D1EBDCBE1}',
          publisher: null, signatureStatus: null,
        },
        {
          name: '\\GoogleSystem\\GoogleUpdater\\GoogleUpdaterTaskSystem157.0.8089.0{A1B2C3D4-0000-4F00-9000-112233445566}',
          path: 'C:\\Program Files (x86)\\Google\\GoogleUpdater\\157.0.8089.0\\updater.exe', author: null,
          enabled: true, triggers: ['At log on', 'Daily at 11:42'], command: '"C:\\Program Files (x86)\\Google\\GoogleUpdater\\157.0.8089.0\\updater.exe" --wake --system',
          publisher: 'Google LLC', signatureStatus: 'signed',
        },
        {
          name: '\\MSIAfterburner', path: 'C:\\Program Files (x86)\\MSI Afterburner\\MSIAfterburner.exe', author: 'MSI',
          enabled: true, triggers: ['At log on'], command: '"C:\\Program Files (x86)\\MSI Afterburner\\MSIAfterburner.exe" /s',
          publisher: 'MICRO-STAR INTERNATIONAL CO., LTD.', signatureStatus: 'signed',
        },
        {
          name: '\\NvTmRep_CrashReport1_{B2FE1952-0186-46C3-BAEC-A80AA35AC5B8}', path: 'C:\\Program Files\\NVIDIA Corporation\\Update Core\\NvTmRep.exe', author: 'NVIDIA Corporation',
          enabled: false, triggers: ['At log on', 'Daily at 18:25'], command: '"C:\\Program Files\\NVIDIA Corporation\\Update Core\\NvTmRep.exe" --crash-report',
          publisher: 'NVIDIA Corporation', signatureStatus: 'signed',
        },
        {
          name: '\\SyncHelperUpdate', path: 'C:\\Users\\Hesh\\AppData\\Roaming\\SyncHelper\\update.exe', author: null,
          enabled: true, triggers: ['At log on'], command: '"C:\\Users\\Hesh\\AppData\\Roaming\\SyncHelper\\update.exe" --silent',
          publisher: null, signatureStatus: 'unsigned',
        },
        {
          name: '\\TaskbarX Hesh', path: 'C:\\Users\\Hesh\\Desktop\\TaskbarX\\TaskbarX.exe', author: 'Hesh',
          enabled: true, triggers: ['At log on'], command: '"C:\\Users\\Hesh\\Desktop\\TaskbarX\\TaskbarX.exe" -tbs=4 -as=cubiceaseinout',
          publisher: null, signatureStatus: 'unknown',
        },
      ],
      services: [
        {
          name: 'NVDisplay.ContainerLocalSystem', displayName: 'NVIDIA Display Container LS',
          path: `${SYS32}\\DriverStore\\FileRepository\\nv_dispi.inf_amd64_1f2a3b4c5d6e7f80\\Display.NvContainer\\NVDisplay.Container.exe`, startType: 'auto',
          publisher: 'NVIDIA Corporation', signatureStatus: 'signed', microsoft: false,
        },
        {
          name: 'GoogleUpdaterService157.0.8089.0', displayName: 'Google Updater Service (GoogleUpdaterService157.0.8089.0)',
          path: 'C:\\Program Files (x86)\\Google\\GoogleUpdater\\157.0.8089.0\\updater.exe', startType: 'auto',
          publisher: 'Google LLC', signatureStatus: 'signed', microsoft: false,
        },
        {
          name: 'CloudflareWARP', displayName: 'Cloudflare WARP', path: 'C:\\Program Files\\Cloudflare\\Cloudflare WARP\\warp-svc.exe', startType: 'auto',
          publisher: 'Cloudflare, Inc.', signatureStatus: 'signed', microsoft: false,
        },
        {
          name: 'Steam Client Service', displayName: 'Steam Client Service', path: 'C:\\Program Files (x86)\\Common Files\\Steam\\steamservice.exe', startType: 'autoDelayed',
          publisher: 'Valve Corp.', signatureStatus: 'signed', microsoft: false,
        },
        {
          name: 'SignalRgbDriver', displayName: 'SignalRgbDriver', path: `${SYS32}\\Drivers\\SignalRgbDriver.sys`, startType: 'auto',
          publisher: 'Microsoft Windows Hardware Compatibility Publisher', signatureStatus: 'signed', microsoft: false,
        },
        {
          name: 'vgk', displayName: 'vgk', path: 'C:\\Program Files\\Riot Vanguard\\vgk.sys', startType: 'system',
          publisher: 'Riot Games, Inc.', signatureStatus: 'signed', microsoft: false,
        },
        {
          name: 'OracleServiceORCL', displayName: 'OracleServiceORCL', path: 'H:\\app\\Hesh\\product\\11.2.0\\dbhome_1\\bin\\ORACLE.EXE', startType: 'auto',
          publisher: null, signatureStatus: 'unsigned', microsoft: false,
        },
        {
          name: 'NetworkDaemon', displayName: 'NetworkDaemon', path: 'C:\\Program Files (x86)\\LagoFast\\NetworkDaemon.exe', startType: 'auto',
          publisher: null, signatureStatus: 'unknown', microsoft: false,
        },
      ],
      logon: params.has('logonok')
        ? { shell: 'explorer.exe', userinit: `${SYS32}\\userinit.exe,`, shellOk: true, userinitOk: true }
        : {
          shell: 'explorer.exe, "C:\\Users\\Hesh\\AppData\\Roaming\\SyncHelper\\shellhost.exe"',
          userinit: `${SYS32}\\userinit.exe,`,
          shellOk: false,
          userinitOk: true,
        },
      imageHijacks: params.has('hijacks')
        ? [
          { image: 'sethc.exe', debugger: `${SYS32}\\cmd.exe` },
          { image: 'taskmgr.exe', debugger: '"C:\\Tools\\Sysinternals\\procexp64.exe"' },
        ]
        : [],
    };
  },
};
