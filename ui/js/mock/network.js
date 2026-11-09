// Orbit / mock/network: the connection table and per-app status for the browser preview
// (network feature, docs/V04-CONTRACT.md). Connections belong to processes in the mock
// snapshot, so a click through to Processes finds them.

function pidOf(rows, name, nth = 0) {
  const hits = rows.filter((r) => r.name.toLowerCase() === name.toLowerCase());
  return hits.length ? hits[Math.min(nth, hits.length - 1)].pid : null;
}

export default {
  async list_connections(_args, ctx) {
    await ctx.delay(150);
    const snap = ctx.snapshot();
    const rows = snap && Array.isArray(snap.processes) ? snap.processes : [];
    const out = [];
    const tcp = (pid, local, lport, remote, rport, state, family = 'ipv4') => {
      if (pid === null) return;
      out.push({ protocol: 'tcp', family, localAddress: local, localPort: lport, remoteAddress: remote, remotePort: rport, state, pid });
    };
    const udp = (pid, local, lport, family = 'ipv4') => {
      if (pid === null) return;
      out.push({ protocol: 'udp', family, localAddress: local, localPort: lport, remoteAddress: null, remotePort: null, state: null, pid });
    };
    const lan = '192.168.1.24';
    const svchost = pidOf(rows, 'svchost.exe', 1);
    tcp(svchost, '0.0.0.0', 135, null, null, 'listen');
    tcp(svchost, '::', 135, null, null, 'listen', 'ipv6');
    tcp(4, '0.0.0.0', 445, null, null, 'listen');
    tcp(4, lan, 139, null, null, 'listen');
    tcp(pidOf(rows, 'lsass.exe'), '0.0.0.0', 49664, null, null, 'listen');
    tcp(pidOf(rows, 'wininit.exe'), '0.0.0.0', 49665, null, null, 'listen');
    tcp(pidOf(rows, 'spoolsv.exe'), '0.0.0.0', 49667, null, null, 'listen');
    tcp(pidOf(rows, 'chrome.exe'), lan, 52114, '142.250.187.206', 443, 'established');
    tcp(pidOf(rows, 'chrome.exe'), lan, 52131, '151.101.65.140', 443, 'established');
    tcp(pidOf(rows, 'chrome.exe', 2), lan, 52140, '104.18.32.47', 443, 'timeWait');
    tcp(pidOf(rows, 'chrome.exe'), '2a02:c7c:5a10:9a00:6d1e:3f2b:91c4:2e7a', 52177, '2a00:1450:4009:81f::200e', 443, 'established', 'ipv6');
    tcp(pidOf(rows, 'Discord.exe'), lan, 52201, '162.159.135.234', 443, 'established');
    tcp(pidOf(rows, 'Code.exe'), lan, 52230, '13.107.42.18', 443, 'established');
    tcp(pidOf(rows, 'Code.exe'), '127.0.0.1', 52231, '127.0.0.1', 52232, 'established');
    tcp(pidOf(rows, 'steam.exe'), lan, 52250, '155.133.248.39', 27017, 'established');
    tcp(pidOf(rows, 'steam.exe'), '127.0.0.1', 27060, null, null, 'listen');
    tcp(pidOf(rows, 'OneDrive.exe'), lan, 52263, '52.123.129.14', 443, 'closeWait');
    udp(pidOf(rows, 'svchost.exe', 3), '0.0.0.0', 5353);
    udp(pidOf(rows, 'svchost.exe', 3), '0.0.0.0', 5355);
    udp(pidOf(rows, 'svchost.exe', 5), '::', 500, 'ipv6');
    udp(pidOf(rows, 'Discord.exe'), '0.0.0.0', 50001);
    udp(pidOf(rows, 'chrome.exe'), '0.0.0.0', 5353);
    return out;
  },

  per_app_network_status(_args, ctx) {
    return ctx.isAdmin()
      ? { available: true, reason: null }
      : { available: false, reason: 'Needs administrator rights.' };
  },
};
