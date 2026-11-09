// Orbit / mock/network: the connection table and per-app status for the browser preview
// (network feature, docs/V04-CONTRACT.md). Connections belong to processes in the mock
// snapshot, so Details and End task find them. Shaped like network.rs: listeners have no
// remote end, UDP rows have no state, rows sorted by local port.
// URL flags: ?netfail (list_connections throws), ?nonet (no connections at all).

const params = typeof location !== 'undefined' ? new URLSearchParams(location.search) : null;
const FAIL = !!params && params.has('netfail');
const EMPTY = !!params && params.has('nonet');

function pidOf(rows, name, nth = 0) {
  const hits = rows.filter((r) => r.name.toLowerCase() === name.toLowerCase());
  return hits.length ? hits[Math.min(nth, hits.length - 1)].pid : null;
}

/** The process named `name` whose command line contains `text`, or null once it has ended. */
function pidByCmd(rows, name, text) {
  const hit = rows.find((r) => r.name.toLowerCase() === name.toLowerCase() && String(r.cmd || '').includes(text));
  return hit ? hit.pid : null;
}

let ephemeral = 52300;

export default {
  async list_connections(_args, ctx) {
    await ctx.delay(120);
    if (FAIL) throw 'Could not read the connection table.';
    if (EMPTY) return [];
    const snap = ctx.snapshot();
    const rows = snap && Array.isArray(snap.processes) ? snap.processes : [];
    const out = [];
    const tcp = (pid, local, lport, remote, rport, state, family = 'ipv4') => {
      if (pid === null) return;
      const listening = state === 'listen';
      out.push({
        protocol: 'tcp', family, localAddress: local, localPort: lport,
        remoteAddress: listening ? null : remote, remotePort: listening ? null : rport, state, pid,
      });
    };
    const udp = (pid, local, lport, family = 'ipv4') => {
      if (pid === null) return;
      out.push({ protocol: 'udp', family, localAddress: local, localPort: lport, remoteAddress: null, remotePort: null, state: null, pid });
    };
    const lan = '192.168.1.24';
    const chrome = pidOf(rows, 'chrome.exe');
    const vite = pidByCmd(rows, 'node.exe', '5173');
    const devServer = pidByCmd(rows, 'node.exe', 'vite.js');

    // Windows itself.
    const rpc = pidOf(rows, 'svchost.exe', 1);
    tcp(rpc, '0.0.0.0', 135, null, null, 'listen');
    tcp(rpc, '::', 135, null, null, 'listen', 'ipv6');
    tcp(4, '0.0.0.0', 445, null, null, 'listen');
    tcp(4, '::', 445, null, null, 'listen', 'ipv6');
    tcp(4, lan, 139, null, null, 'listen');
    udp(4, lan, 137);
    udp(4, lan, 138);
    tcp(pidOf(rows, 'lsass.exe'), '0.0.0.0', 49664, null, null, 'listen');
    tcp(pidOf(rows, 'wininit.exe'), '0.0.0.0', 49665, null, null, 'listen');
    tcp(pidOf(rows, 'spoolsv.exe'), '0.0.0.0', 49667, null, null, 'listen');
    udp(pidOf(rows, 'svchost.exe', 3), '0.0.0.0', 5353);
    udp(pidOf(rows, 'svchost.exe', 3), '::', 5353, 'ipv6');
    udp(pidOf(rows, 'svchost.exe', 3), '0.0.0.0', 5355);
    udp(pidOf(rows, 'svchost.exe', 4), '0.0.0.0', 123);
    udp(pidOf(rows, 'svchost.exe', 5), '0.0.0.0', 500);
    udp(pidOf(rows, 'svchost.exe', 5), '::', 500, 'ipv6');
    udp(pidOf(rows, 'svchost.exe', 6), '127.0.0.1', 1900);
    udp(pidOf(rows, 'svchost.exe', 6), 'fe80::38e7:1d28:3f57:fef4%11', 1900, 'ipv6');

    // Local dev servers and the browser tabs talking to them.
    tcp(devServer, '0.0.0.0', 3000, null, null, 'listen');
    tcp(devServer, '::', 3000, null, null, 'listen', 'ipv6');
    tcp(devServer, '127.0.0.1', 3000, '127.0.0.1', 52410, 'established');
    tcp(chrome, '127.0.0.1', 52410, '127.0.0.1', 3000, 'established');
    tcp(vite, '127.0.0.1', 5173, null, null, 'listen');
    tcp(vite, '::1', 5173, null, null, 'listen', 'ipv6');
    tcp(vite, '::1', 5173, '::1', 52418, 'established', 'ipv6');
    tcp(chrome, '::1', 52418, '::1', 5173, 'established', 'ipv6');
    tcp(vite, '127.0.0.1', 24678, null, null, 'listen');

    // Browser and apps on the internet.
    tcp(chrome, lan, 52114, '142.250.187.206', 443, 'established');
    tcp(chrome, lan, 52131, '151.101.65.140', 443, 'established');
    tcp(pidOf(rows, 'chrome.exe', 2), lan, 52140, '104.18.32.47', 443, 'established');
    tcp(chrome, '2a02:c7c:5a10:9a00:6d1e:3f2b:91c4:2e7a', 52177, '2a00:1450:4009:81f::200e', 443, 'established', 'ipv6');
    udp(pidOf(rows, 'chrome.exe', 2), '0.0.0.0', 5353);
    udp(chrome, '0.0.0.0', 57621);
    tcp(pidOf(rows, 'Discord.exe'), lan, 52201, '162.159.135.234', 443, 'established');
    udp(pidOf(rows, 'Discord.exe'), '0.0.0.0', 50001);
    tcp(pidOf(rows, 'Code.exe'), lan, 52230, '13.107.42.18', 443, 'established');
    tcp(pidOf(rows, 'Code.exe'), '127.0.0.1', 52231, '127.0.0.1', 52232, 'established');
    tcp(pidOf(rows, 'Code.exe'), '127.0.0.1', 52232, '127.0.0.1', 52231, 'established');
    tcp(pidOf(rows, 'steam.exe'), lan, 52250, '155.133.248.39', 27017, 'established');
    tcp(pidOf(rows, 'steam.exe'), '127.0.0.1', 27060, null, null, 'listen');
    tcp(pidOf(rows, 'OneDrive.exe'), lan, 52263, '52.123.129.14', 443, 'closeWait');

    // Short-lived connections come and go between refreshes, as on a real PC.
    const n = Math.floor(Math.random() * 3);
    for (let i = 0; i < n; i++) {
      ephemeral = ephemeral >= 52390 ? 52300 : ephemeral + 1;
      tcp(0, lan, ephemeral, '104.18.32.47', 443, 'timeWait');
    }
    if (Math.random() < 0.5) tcp(chrome, lan, 52299, '140.82.112.25', 443, 'synSent');

    out.sort((a, b) => a.localPort - b.localPort || a.protocol.localeCompare(b.protocol) || a.family.localeCompare(b.family) || a.pid - b.pid);
    return out;
  },

  per_app_network_status(_args, ctx) {
    return ctx.isAdmin()
      ? { available: true, reason: null }
      : { available: false, reason: 'Needs administrator rights. Use Run as admin.' };
  },
};
