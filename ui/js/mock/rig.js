// Orbit / mock/rig: the My Rig spec sheet for the browser preview (rig.rs,
// docs/CONTRACT.md "v0.6"). A gaming laptop with a second screen, so every card has
// something to show: two GPUs (mock/gpu.js), a brand PC name, two drives, two displays.
// URL flag: ?rigfail (get_rig_specs throws).

import { ADAPTERS } from './gpu.js';

const GIB = 1024 ** 3;

function flag(name) {
  try {
    return new URLSearchParams(location.search).has(name);
  } catch {
    return false;
  }
}

export default {
  async get_rig_specs(_args, ctx) {
    await ctx.delay(220);
    if (flag('rigfail')) throw "Could not read this PC's specs.";
    return {
      cpu: { name: '13th Gen Intel(R) Core(TM) i7-13700H', cores: 14, threads: 20, baseMhz: 2400, sockets: 1 },
      memory: { total: 32 * GIB, modules: [16 * GIB, 16 * GIB], speed: 5200, kind: 'DDR5', slotsUsed: 2, slotsTotal: 2 },
      gpus: flag('nogpu') ? [] : ADAPTERS.map((a) => ({ ...a, sharedMemory: Math.round(a.sharedMemory) })),
      board: {
        maker: 'LENOVO',
        model: 'LNVNB161216',
        biosVendor: 'LENOVO',
        biosVersion: 'LPCN49WW',
        biosDate: '03/14/2025',
        systemMaker: null,
        systemModel: 'Legion Pro 5 16IRX8',
      },
      drives: [
        { name: 'SAMSUNG MZVL21T0HCLR-00BL2', size: 1024209543168, media: 'SSD', bus: 'NVMe' },
        { name: 'WD Elements 25A3', size: 2000365289472, media: 'HDD', bus: 'USB' },
      ],
      displays: [
        { name: null, width: 2560, height: 1600, refreshHz: 240, primary: true, internal: true },
        { name: 'DELL U2723QE', width: 3840, height: 2160, refreshHz: 59.94, primary: false, internal: false },
      ],
      windows: { edition: 'Windows 11 Home', version: '24H2', build: '26100.2314' },
      readAt: Math.floor(Date.now() / 1000),
    };
  },
};
