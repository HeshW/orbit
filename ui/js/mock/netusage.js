// Orbit / mock/netusage: Network > Usage for the browser preview (netusage feature,
// docs/features/network-usage.md). Machine totals come from a fixed seed, so reloads show
// the same numbers. Per-app bytes are App history's (mock/history.js): the last hour and
// 24 hours only once the mock has "restarted as admin", 7 and 30 days from the history
// days that have network bytes (9 to 11 days ago, plus today as admin), like netusage.rs.
// URL flags: ?nonetusage (nothing recorded yet), ?netusagefail (get_network_usage throws).

import history, { historyCleared } from './history.js';

const MB = 1024 ** 2;
const DAY = 86400;
const PERIODS = { hour: 3600, day: DAY, week: 7, month: 30 };

function flag(name) {
  try {
    return new URLSearchParams(location.search).has(name);
  } catch {
    return false;
  }
}

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

let machine = null; // machine[d] = { rx, tx } for the day d days ago

function midnight() {
  const now = new Date();
  return new Date(now.getFullYear(), now.getMonth(), now.getDate()).getTime() / 1000;
}

function buildMachine() {
  const rand = rng(0x5e7);
  const todayShare = Math.min(1, Math.max(0.05, (Date.now() / 1000 - midnight()) / (16 * 3600)));
  const out = [];
  for (let d = 0; d < 30; d++) {
    const f = d === 0 ? todayShare : 1;
    out.push({ rx: (1200 + rand() * 3300) * MB * f, tx: (90 + rand() * 520) * MB * f });
  }
  return out;
}

export default {
  async get_network_usage({ period } = {}, ctx) {
    await ctx.delay(110);
    if (flag('netusagefail')) throw 'Could not read network usage.';
    if (!(period in PERIODS)) throw 'Pick a period: last hour, 24 hours, 7 days or 30 days.';
    const now = Math.floor(Date.now() / 1000);
    const short = period === 'hour' || period === 'day';
    const from = short ? now - PERIODS[period] : Math.floor(midnight() - (PERIODS[period] - 1) * DAY);
    const empty = { period, from, rxBytes: 0, txBytes: 0, since: null, recording: true, apps: [] };
    if (flag('nonetusage') || historyCleared()) return empty;
    if (!machine) machine = buildMachine();

    let rx = 0;
    let tx = 0;
    let since = from;
    if (short) {
      // Today so far plus the rest of yesterday for 24 hours; a slice of today for an hour.
      const hoursToday = Math.max(1, (now - midnight()) / 3600);
      const share = period === 'hour' ? 1 / Math.min(16, hoursToday) : 1;
      rx = machine[0].rx * share + (period === 'day' ? machine[1].rx * 0.4 : 0);
      tx = machine[0].tx * share + (period === 'day' ? machine[1].tx * 0.4 : 0);
    } else {
      for (let d = 0; d < PERIODS[period]; d++) {
        rx += machine[d].rx;
        tx += machine[d].tx;
      }
      since = from + 8 * 3600;
    }

    let apps = [];
    const admin = ctx.isAdmin();
    if (!short || admin) {
      const rows = await history.get_history({ days: short ? 1 : PERIODS[period] }, ctx);
      const scale = period === 'hour' ? 1 / Math.min(16, Math.max(1, (now - midnight()) / 3600)) : 1;
      apps = (Array.isArray(rows) ? rows : [])
        .map((r) => ({ key: r.key, name: r.name, exe: r.exe, rxBytes: Math.round(r.netRxBytes * scale), txBytes: Math.round(r.netTxBytes * scale) }))
        .filter((a) => a.rxBytes + a.txBytes > 0)
        .sort((a, b) => (b.rxBytes + b.txBytes) - (a.rxBytes + a.txBytes) || a.key.localeCompare(b.key));
      // Per-app counts never add up to more than the machine moved.
      const appRx = apps.reduce((s, a) => s + a.rxBytes, 0);
      const appTx = apps.reduce((s, a) => s + a.txBytes, 0);
      rx = Math.max(rx, appRx * 1.08);
      tx = Math.max(tx, appTx * 1.08);
    }
    return { period, from, rxBytes: Math.round(rx), txBytes: Math.round(tx), since, recording: true, apps };
  },
};
