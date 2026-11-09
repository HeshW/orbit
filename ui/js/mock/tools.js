// Orbit / mock/tools: Find file locker for the browser preview (tools feature,
// docs/V04-CONTRACT.md). Lockers are processes from the mock snapshot.

function rowsOf(ctx) {
  const snap = ctx.snapshot();
  return snap && Array.isArray(snap.processes) ? snap.processes : [];
}

function locker(row, appType) {
  return row ? { pid: row.pid, name: row.name, exe: row.exe, appType } : null;
}

export default {
  async find_file_lockers({ path }, ctx) {
    const p = String(path || '').trim();
    if (!p) throw 'Pick a file first.';
    await ctx.delay(250);
    const rows = rowsOf(ctx);
    const find = (name) => rows.find((r) => r.name.toLowerCase() === name.toLowerCase());
    const out = [];
    if (/\.(prproj|mp4|mov|wav)$/i.test(p) || /\\edits\\/i.test(p)) {
      out.push(locker(find('Adobe Premiere Pro.exe'), 'app'));
    }
    if (/\.(log|etl)$/i.test(p)) out.push(locker(find('svchost.exe'), 'service'));
    if (/\.(js|json|md|rs|css|html)$/i.test(p)) out.push(locker(find('Code.exe'), 'app'));
    // Explorer often holds a file open for its preview pane.
    if (/\.(pdf|docx|png|jpg|mp4)$/i.test(p)) out.push(locker(find('explorer.exe'), 'explorer'));
    return out.filter(Boolean);
  },
};
