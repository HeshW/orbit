// Orbit / mock/autoruns: scheduled tasks, services, logon keys and image hijacks for the
// browser preview (autoruns feature, docs/V04-CONTRACT.md).

const SYS32 = 'C:\\Windows\\System32';

export default {
  async list_autoruns(_args, ctx) {
    await ctx.delay(400);
    return {
      tasks: [
        {
          name: '\\Microsoft\\Windows\\Defrag\\ScheduledDefrag', path: `${SYS32}\\defrag.exe`, author: 'Microsoft Corporation',
          enabled: true, triggers: ['Weekly on Wednesday at 01:00'], command: `${SYS32}\\defrag.exe -c -h -o -$`,
          publisher: 'Microsoft Windows', signatureStatus: 'signed',
        },
        {
          name: '\\Microsoft\\Windows\\WindowsUpdate\\Scheduled Start', path: `${SYS32}\\sc.exe`, author: 'Microsoft Corporation',
          enabled: true, triggers: ['Daily at 03:00', 'At system start'], command: `${SYS32}\\sc.exe start wuauserv`,
          publisher: 'Microsoft Windows', signatureStatus: 'signed',
        },
        {
          name: '\\GoogleUpdateTaskMachineUA', path: 'C:\\Program Files (x86)\\Google\\Update\\GoogleUpdate.exe', author: 'Google LLC',
          enabled: true, triggers: ['Daily at 11:42', 'Every hour'], command: '"C:\\Program Files (x86)\\Google\\Update\\GoogleUpdate.exe" /ua /installsource scheduler',
          publisher: 'Google LLC', signatureStatus: 'signed',
        },
        {
          name: '\\NvTmRep_CrashReport1_{B2FE1952-0186-46C3-BAEC-A80AA35AC5B8}', path: 'C:\\Program Files\\NVIDIA Corporation\\Update Core\\NvTmRep.exe', author: 'NVIDIA Corporation',
          enabled: false, triggers: ['Daily at 18:25'], command: '"C:\\Program Files\\NVIDIA Corporation\\Update Core\\NvTmRep.exe" --crash-report',
          publisher: 'NVIDIA Corporation', signatureStatus: 'signed',
        },
        {
          name: '\\SyncHelperUpdate', path: 'C:\\Users\\Hesh\\AppData\\Roaming\\SyncHelper\\update.exe', author: null,
          enabled: true, triggers: ['At log on'], command: '"C:\\Users\\Hesh\\AppData\\Roaming\\SyncHelper\\update.exe" --silent',
          publisher: null, signatureStatus: 'unsigned',
        },
      ],
      services: [
        {
          name: 'WSearch', displayName: 'Windows Search', path: `${SYS32}\\SearchIndexer.exe /Embedding`, startType: 'autoDelayed',
          publisher: 'Microsoft Windows', signatureStatus: 'signed', microsoft: true,
        },
        {
          name: 'Spooler', displayName: 'Print Spooler', path: `${SYS32}\\spoolsv.exe`, startType: 'auto',
          publisher: 'Microsoft Windows', signatureStatus: 'signed', microsoft: true,
        },
        {
          name: 'NVDisplay.ContainerLocalSystem', displayName: 'NVIDIA Display Container LS', path: `${SYS32}\\DriverStore\\FileRepository\\nv_dispi.inf_amd64_1f2a3b4c5d6e7f80\\Display.NvContainer\\NVDisplay.Container.exe -s NVDisplay.ContainerLocalSystem`, startType: 'auto',
          publisher: 'NVIDIA Corporation', signatureStatus: 'signed', microsoft: false,
        },
        {
          name: 'Steam Client Service', displayName: 'Steam Client Service', path: '"C:\\Program Files (x86)\\Common Files\\Steam\\steamservice.exe" /RunAsService', startType: 'manual',
          publisher: 'Valve Corp.', signatureStatus: 'signed', microsoft: false,
        },
        {
          name: 'gupdate', displayName: 'Google Update Service (gupdate)', path: '"C:\\Program Files (x86)\\Google\\Update\\GoogleUpdate.exe" /svc', startType: 'autoDelayed',
          publisher: 'Google LLC', signatureStatus: 'signed', microsoft: false,
        },
      ],
      logon: {
        shell: 'explorer.exe',
        userinit: `${SYS32}\\userinit.exe,`,
        shellOk: true,
        userinitOk: true,
      },
      imageHijacks: [],
    };
  },
};
