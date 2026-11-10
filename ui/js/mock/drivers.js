// Orbit / mock: driver check (src-tauri/src/drivers.rs) for the browser preview. A fixed set
// of devices with every flag (problem code, unsigned, old, turned off), a Windows Update
// driver search that takes a few seconds, and a fake install that never touches anything.
// Mirrors the Rust rules: old = third-party (oemNN.inf) and 5+ years; 22 / 45 are "off".

const DAY = 86400;
const YEAR = 365.25 * DAY;
const now = () => Math.floor(Date.now() / 1000);
const ago = (years) => Math.floor(now() - years * YEAR);

const MAKERS = [
  ['NVIDIA', 'https://www.nvidia.com/en-us/drivers/', ['nvidia']],
  ['AMD', 'https://www.amd.com/en/support/download/drivers.html', ['amd', 'advanced micro devices']],
  ['Intel', 'https://www.intel.com/content/www/us/en/support/detect.html', ['intel']],
  ['Realtek', 'https://www.realtek.com/Download/Overview', ['realtek']],
  ['Logitech', 'https://support.logi.com/', ['logitech']],
  ['ASUS', 'https://www.asus.com/support/download-center/', ['asus', 'asustek']],
];

function vendorFor(...names) {
  for (const n of names) {
    const hay = ` ${String(n || '').toLowerCase().replace(/[^a-z0-9]+/g, ' ')} `;
    const m = MAKERS.find(([, , words]) => words.some((w) => hay.includes(` ${w} `)));
    if (m) return { name: m[0], url: m[1] };
  }
  return null;
}

const PROBLEM = {
  10: 'This device cannot start. Updating its driver often fixes this.',
  22: 'This device is turned off in Device Manager.',
  28: 'No driver is installed for this device.',
  43: 'Windows stopped this device because it reported problems.',
};

// [name, class, manufacturer, provider, version, years old, inf, signature, problem]
const DEVICES = [
  ['NVIDIA GeForce RTX 3060', 'Display adapters', 'NVIDIA', 'NVIDIA', '31.0.15.4633', 0.9, 'oem61.inf', 'whql', null],
  ['Intel(R) UHD Graphics 770', 'Display adapters', 'Intel Corporation', 'Intel Corporation', '31.0.101.4502', 1.6, 'oem44.inf', 'whql', null],
  ['Realtek PCIe 2.5GbE Family Controller', 'Network adapters', 'Realtek', 'Realtek', '10.38.1201.2019', 6.1, 'oem12.inf', 'whql', null],
  ['Intel(R) Wi-Fi 6 AX201 160MHz', 'Network adapters', 'Intel Corporation', 'Intel', '22.250.1.2', 0.7, 'oem88.inf', 'whql', 22],
  ['WAN Miniport (IP)', 'Network adapters', 'Microsoft', 'Microsoft', '10.0.19041.1', 19.4, 'netrasa.inf', 'windows', null],
  ['Realtek High Definition Audio', 'Sound, video and game controllers', 'Realtek', 'Realtek Semiconductor Corp.', '6.0.9235.1', 2.1, 'oem20.inf', 'whql', null],
  ['USB Audio Device', 'Sound, video and game controllers', '(Generic USB Audio)', 'Microsoft', '10.0.19041.3636', 19.4, 'wdma_usb.inf', 'windows', null],
  ['Logitech BRIO', 'Cameras', 'Logitech', 'Logitech', '2.3.3.46', 7.2, 'oem31.inf', 'whql', 10],
  ['Logitech G Pro Mouse', 'Mice and other pointing devices', 'Logitech', 'Logitech', '1.10.78.0', 5.8, 'oem37.inf', 'authenticode', null],
  ['HID-compliant mouse', 'Mice and other pointing devices', 'Microsoft', 'Microsoft', '10.0.19041.1', 19.4, 'msmouse.inf', 'windows', null],
  ['HID Keyboard Device', 'Keyboards', '(Standard keyboards)', 'Microsoft', '10.0.19041.3636', 19.4, 'keyboard.inf', 'windows', null],
  ['ASUS Aura LED Controller', 'Human Interface Devices', 'ASUSTeK Computer Inc.', 'ASUSTeK Computer Inc.', '1.0.0.12', 3.3, 'oem52.inf', 'unsigned', null],
  ['Fingerprint Sensor', 'Other devices', null, null, null, null, null, null, 28],
  ['Samsung SSD 980 PRO 500GB', 'Disk drives', '(Standard disk drives)', 'Microsoft', '10.0.19041.3636', 19.4, 'disk.inf', 'windows', null],
  ['Standard NVM Express Controller', 'Storage controllers', 'Standard NVM Express Controller', 'Microsoft', '10.0.19041.3636', 19.4, 'stornvme.inf', 'windows', null],
  ['Intel(R) Serial IO I2C Host Controller - 7AF8', 'System devices', 'INTEL', 'INTEL', '30.100.2129.8', 4.4, 'oem7.inf', 'whql', null],
  ['Intel(R) Management Engine Interface #1', 'System devices', 'Intel', 'Intel', '2251.4.2.0', 2.8, 'oem15.inf', 'whql', 43],
  ['High precision event timer', 'System devices', '(Standard system devices)', 'Microsoft', '10.0.19041.3636', 19.4, 'machine.inf', 'windows', null],
  ['Generic USB Hub', 'Universal Serial Bus controllers', '(Generic USB Hub)', 'Microsoft', '10.0.19041.3636', 19.4, 'usb.inf', 'windows', null],
  ['Bluetooth Device (RFCOMM Protocol TDI)', 'Bluetooth', 'Microsoft', 'Microsoft', '10.0.19041.1', 19.4, 'tdibth.inf', 'windows', null],
];

const SIGNERS = {
  windows: 'Microsoft Windows',
  whql: 'Microsoft Windows Hardware Compatibility Publisher',
  authenticode: 'Logitech Inc',
  unsigned: null,
};
const RANK = ['problem', 'unsigned', 'old', 'off', 'ok'];
const SOFT = new Set([22, 45]);

function device(row, i) {
  const [name, cls, manufacturer, provider, version, years, inf, signature, problem] = row;
  const inbox = !!inf && !/^oem/i.test(inf);
  const signed = signature == null ? null : signature !== 'unsigned';
  const old = !!inf && !inbox && years != null && years >= 5;
  const flags = [];
  if (problem != null && !SOFT.has(problem)) flags.push('problem');
  if (signed === false) flags.push('unsigned');
  if (old) flags.push('old');
  if (problem != null && SOFT.has(problem)) flags.push('off');
  return {
    id: `PCI\\VEN_MOCK&DEV_${String(1000 + i)}\\${i}`,
    name, class: cls, manufacturer, provider, version,
    date: years == null ? null : ago(years), inf, inbox, signed,
    signer: signature ? SIGNERS[signature] : null, signature,
    problem, problemText: problem != null ? PROBLEM[problem] || `This device reports a problem (code ${problem}).` : null,
    status: flags[0] || 'ok', flags,
    vendor: vendorFor(provider, manufacturer),
    updateId: null,
  };
}

const OFFERS = [
  {
    id: '8a7d3c1e-5b2f-4e6a-9c0d-1f2e3a4b5c6d', title: 'Realtek - Net - 10.68.815.2023',
    class: 'Net', provider: 'Realtek', manufacturer: 'Realtek', model: 'Realtek PCIe 2.5GbE Family Controller',
    hardwareId: 'PCI\\VEN_10EC&DEV_8125', date: ago(2.2), optional: false, downloaded: false,
    deviceName: 'Realtek PCIe 2.5GbE Family Controller',
  },
  {
    id: '2c4e6a8b-0d1f-4a3c-8e5b-7d9f1a3c5e7b', title: 'Logitech - Image - 2.3.4.12',
    class: 'Image', provider: 'Logitech', manufacturer: 'Logitech', model: 'Logitech BRIO',
    hardwareId: 'USB\\VID_046D&PID_085E', date: ago(0.8), optional: true, downloaded: false,
    deviceName: 'Logitech BRIO',
  },
];

let search = { state: 'idle', offers: [], checkedAt: null, online: false, error: null, installing: null, lastInstall: null };
let searchTimer = null;
const installed = new Set();

function startSearch(online) {
  if (search.state === 'checking' || search.installing) return;
  search = { ...search, state: 'checking' };
  clearTimeout(searchTimer);
  searchTimer = setTimeout(() => {
    search = { ...search, state: 'ready', offers: OFFERS.filter((o) => !installed.has(o.id)), checkedAt: now(), online, error: null };
  }, online ? 5000 : 2500);
}

function list(ctx) {
  const devices = DEVICES.map(device);
  for (const o of search.offers) {
    const d = devices.find((x) => x.name === o.deviceName);
    if (d) {
      d.updateId = o.id;
      o.deviceId = d.id;
    }
  }
  devices.sort((a, b) => RANK.indexOf(a.status) - RANK.indexOf(b.status)
    || a.class.localeCompare(b.class) || a.name.localeCompare(b.name));
  const counts = { total: devices.length, problem: 0, unsigned: 0, old: 0, off: 0 };
  for (const d of devices) for (const f of d.flags) counts[f]++;
  return {
    scannedAt: now(), isAdmin: ctx.isAdmin(), oldYears: 5, devices, hidden: 41, counts,
    system: { manufacturer: 'ASUSTeK COMPUTER INC.', model: 'ROG STRIX B660-F GAMING WIFI', vendor: vendorFor('ASUSTeK') },
    updates: { ...search, offers: search.offers.map((o) => ({ ...o })) },
  };
}

export default {
  async list_drivers(args, ctx) {
    await ctx.delay(600);
    if (search.state === 'idle') startSearch(false);
    return list(ctx);
  },

  get_driver_updates({ refresh } = {}) {
    if (refresh) startSearch(true);
    else if (search.state === 'idle') startSearch(false);
    return { ...search };
  },

  // Fake: waits, then reports success with a restart. Nothing is installed anywhere.
  async install_driver_update({ id } = {}, ctx) {
    const offer = search.offers.find((o) => o.id === id);
    if (!offer) throw 'Windows Update no longer offers this driver. Check again.';
    if (!ctx.isAdmin()) throw 'Installing a driver needs administrator rights. Restart Orbit as administrator first.';
    if (search.installing) throw 'Another driver is being installed. Wait for it to finish.';
    search = { ...search, installing: id };
    await ctx.delay(4000);
    installed.add(id);
    const result = {
      id, title: offer.title, ok: true, result: 'succeeded', restartRequired: true,
      message: 'Installed. Restart the PC to finish.', at: now(),
    };
    search = { ...search, installing: null, lastInstall: result, offers: search.offers.filter((o) => o.id !== id) };
    console.info(`[orbit mock] pretended to install ${offer.title}`);
    return result;
  },
};
