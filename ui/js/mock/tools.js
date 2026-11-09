// Orbit / mock/tools: Find file locker for the browser preview (tools feature,
// docs/V04-CONTRACT.md). Lockers are processes from the mock snapshot.
//
// Sample paths:
//   E:\Work\Edits\client-reel.prproj   Premiere (not responding) + Explorer
//   E:\Work\LegacyHub\apps\orbit       a folder: Code + Explorer
//   C:\Windows\Logs\CBS\CBS.log        a service (svchost)
//   C:\Users\you\Desktop\notes.txt     nothing has it open
//   ...missing...                      not found;  ...denied...  access denied

const NOT_FOUND = "Orbit can't find that file or folder. Check the path and try again.";

function rowsOf(ctx) {
  const snap = ctx.snapshot();
  return snap && Array.isArray(snap.processes) ? snap.processes : [];
}

function locker(row, appType) {
  return row ? { pid: row.pid, name: row.name, exe: row.exe || null, appType } : null;
}

export default {
  async find_file_lockers({ path }, ctx) {
    let p = String(path || '').trim();
    if (p.length >= 2 && p.startsWith('"') && p.endsWith('"')) p = p.slice(1, -1).trim();
    if (!p) throw 'Type the path of a file or folder.';
    if (!/^([a-z]:\\|\\\\)/i.test(p)) throw 'Type the full path, starting with a drive letter like C:\\.';
    await ctx.delay(250);
    if (/missing|nosuch/i.test(p)) throw NOT_FOUND;
    if (/denied/i.test(p)) throw 'Access denied. Try Restart as administrator.';
    const rows = rowsOf(ctx);
    const find = (name) => rows.find((r) => r.name.toLowerCase() === name.toLowerCase());
    const out = [];
    const isFolder = !/\.[a-z0-9]{1,6}$/i.test(p);
    if (isFolder) {
      if (/\\work\b/i.test(p)) out.push(locker(find('Code.exe'), 'app'));
      out.push(locker(find('explorer.exe'), 'explorer'));
      return out.filter(Boolean);
    }
    if (/\.(prproj|mp4|mov|wav)$/i.test(p) || /\\edits\\/i.test(p)) {
      out.push(locker(find('Adobe Premiere Pro.exe'), 'app'));
    }
    if (/\.(log|etl)$/i.test(p)) out.push(locker(find('svchost.exe'), 'service'));
    if (/\.(js|json|md|rs|css|html)$/i.test(p)) out.push(locker(find('Code.exe'), 'app'));
    // Explorer often holds a file open for its preview pane.
    if (/\.(pdf|docx|png|jpg|mp4|prproj)$/i.test(p)) out.push(locker(find('explorer.exe'), 'explorer'));
    return out.filter(Boolean);
  },
};
