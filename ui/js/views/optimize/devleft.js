// Optimize > Cleanup > Developer leftovers (1.2, roadmap 14b and 14c).
// The user adds projects folders (a folder picker opened by Rust); devleft_scan lists old
// node_modules / target folders with ids, nothing ticked. devleft_hold moves the ticked ones to a
// hold folder (ids only, never paths); held items can be restored or deleted now, and Rust deletes
// them after 7 days. Mounted by cleanup.js into its own panel.
import { call } from '../../api.js';
import { escapeHtml, bytes, time } from '../../format.js';
import { toast, confirmDialog } from '../../app.js';

const text = (s) => escapeHtml(String(s ?? ''));
const icon = (id) => `<svg class="icon" aria-hidden="true"><use href="#${id}"/></svg>`;
const plural = (n, one, many = `${one}s`) => `${Number(n || 0).toLocaleString()} ${n === 1 ? one : many}`;
const KIND = { node: 'node_modules', rust: 'target' };

let el = null;
const dl = { state: null, scan: null, picks: new Set(), busy: false, error: null };

function errMsg(err) {
  if (typeof err === 'string') return err;
  if (err && typeof err.message === 'string') return err.message;
  return 'Something went wrong.';
}

async function refresh({ rescan = true } = {}) {
  try {
    dl.state = await call('devleft_state');
    if (rescan) {
      dl.scan = dl.state.folders.length ? await call('devleft_scan') : null;
      dl.picks = new Set();
    }
    dl.error = null;
  } catch (err) {
    dl.error = errMsg(err);
  }
  render();
}

async function run(fn) {
  if (dl.busy) return;
  dl.busy = true;
  render();
  try {
    await fn();
  } catch (err) {
    toast(errMsg(err), 'error');
  } finally {
    dl.busy = false;
  }
  await refresh();
}

function report(r, verb) {
  if (r.done) toast(`${verb} ${plural(r.done, 'folder')} (${bytes(r.bytes)}).`, 'success');
  for (const k of r.kept) toast(`${k.path ? `${k.path}: ` : ''}${k.reason}`, 'error');
  if (!r.done && !r.kept.length) toast('Nothing changed.', 'info');
}

async function hold() {
  const list = (dl.scan?.projects || []).filter((p) => dl.picks.has(p.id));
  if (!list.length) return;
  const total = list.reduce((a, p) => a + p.bytes, 0);
  const ok = await confirmDialog({
    title: `Move ${bytes(total)} to the hold?`,
    body: `${list.map((p) => p.name).join(', ')}. Orbit keeps them for ${dl.state.holdDays} days so you can restore them, then deletes them.`,
    confirmLabel: 'Move to hold',
  });
  if (!ok) return;
  await run(async () => report(await call('devleft_hold', { ids: list.map((p) => p.id) }), 'Moved'));
}

async function deleteNow(h) {
  const ok = await confirmDialog({
    title: `Delete ${bytes(h.bytes)} for good?`,
    body: `${h.original} is deleted now instead of in ${dl.state.holdDays} days.`,
    confirmLabel: 'Delete now',
    danger: true,
  });
  if (!ok) return;
  await run(async () => report(await call('devleft_delete_held', { ids: [h.id] }), 'Deleted'));
}

function projectHtml(p) {
  const on = dl.picks.has(p.id);
  return `<li class="cl-cat${on ? ' is-on' : ''}" title="${text(p.path)}">
      <label class="cl-cat-main">
        <input type="checkbox" class="cl-check" data-dl-pick="${text(p.id)}"${on ? ' checked' : ''}${dl.busy ? ' disabled' : ''}>
        <span class="cl-cat-text">
          <span class="cl-cat-title">${text(p.name)}<span class="chip chip--sm">${text(KIND[p.kind] || p.kind)}</span></span>
          <span class="cl-cat-line">${text(p.project)}, last changed ${text(time(p.lastChange))}</span>
        </span>
      </label>
      <span class="cl-size">${text(bytes(p.bytes))}<small>${text(plural(p.files, 'file'))}</small></span>
    </li>`;
}

function heldHtml(h) {
  return `<li class="cl-cat" title="${text(h.original)}">
      <span class="cl-cat-main">
        <span class="cl-cat-text">
          <span class="cl-cat-title">${text(h.original)}</span>
          <span class="cl-cat-line">Deleted for good at ${text(time(h.expiresAt))}.</span>
        </span>
      </span>
      <span class="cl-size">${text(bytes(h.bytes))}</span>
      <button type="button" class="btn btn--ghost btn--sm" data-dl-restore="${text(h.id)}"${dl.busy ? ' disabled' : ''}>Restore</button>
      <button type="button" class="btn btn--ghost btn--sm" data-dl-delete="${text(h.id)}"${dl.busy ? ' disabled' : ''}>Delete now</button>
    </li>`;
}

function render() {
  if (!el) return;
  const s = dl.state;
  let body = '';
  if (dl.error) body += `<p class="cl-folder-error">${text(dl.error)}</p>`;
  if (!s) {
    el.innerHTML = body;
    return;
  }
  const folders = s.folders.map((f) => `<span class="chip cl-dev-folder" title="${text(f.problem || f.path)}">${icon('i-folder')}${text(f.path)}<button type="button" class="icon-btn" data-dl-remove="${text(f.path)}" aria-label="Remove ${text(f.path)}" title="Remove"${dl.busy ? ' disabled' : ''}>${icon('i-close')}</button></span>`).join('');
  body += `<div class="cl-pick cl-dev-folders" data-dl-folders>${folders}
      <button type="button" class="btn btn--ghost btn--sm" data-dl-add${dl.busy ? ' disabled' : ''}>Add folder</button></div>`;
  for (const f of s.folders.filter((x) => x.problem)) body += `<p class="cl-cat-sub cl-cat-sub--warn">${text(f.path)}: ${text(f.problem)}</p>`;
  if (!s.folders.length) {
    body += `<p class="lv-help cl-folder-help">Add a projects folder, like E:\\Work, and Orbit lists node_modules and target folders untouched for ${s.untouchedDays} days.</p>`;
  } else if (dl.scan) {
    const list = dl.scan.projects;
    body += list.length
      ? `<ul class="cl-cats" aria-label="Developer leftovers">${list.map(projectHtml).join('')}</ul>`
      : `<p class="lv-help cl-folder-help">No build folders untouched for ${s.untouchedDays} days.</p>`;
    if (dl.scan.recent) body += `<p class="lv-help">${text(plural(dl.scan.recent, 'project'))} changed in the last ${s.untouchedDays} days and stay.</p>`;
    const picked = list.filter((p) => dl.picks.has(p.id));
    const total = picked.reduce((a, p) => a + p.bytes, 0);
    body += `<div class="cl-foot">
        <span class="cl-foot-total">${picked.length ? `<strong>${text(bytes(total))}</strong> in ${plural(picked.length, 'project')}` : 'Tick what to move to the hold'}</span>
        <button type="button" class="btn btn--primary" data-dl-hold${picked.length && !dl.busy ? '' : ' disabled'}>Move to hold</button>
      </div>`;
  }
  if (s.held.length) {
    body += `<h3 class="micro cl-panel-title cl-dev-held-title">Held for ${s.holdDays} days</h3>
      <ul class="cl-cats" aria-label="Held folders" data-dl-held>${s.held.map(heldHtml).join('')}</ul>`;
  }
  el.innerHTML = body;
}

export function mountDevLeft(container) {
  el = container;
  el.addEventListener('change', (ev) => {
    const pick = ev.target.closest('[data-dl-pick]');
    if (!pick) return;
    if (pick.checked) dl.picks.add(pick.dataset.dlPick);
    else dl.picks.delete(pick.dataset.dlPick);
    render();
  });
  el.addEventListener('click', (ev) => {
    const t = ev.target;
    const held = (id) => dl.state?.held.find((h) => h.id === id);
    const rm = t.closest('[data-dl-remove]');
    const restore = t.closest('[data-dl-restore]');
    const del = t.closest('[data-dl-delete]');
    if (t.closest('[data-dl-add]')) run(() => call('devleft_add_folder'));
    else if (rm) run(() => call('devleft_remove_folder', { path: rm.dataset.dlRemove }));
    else if (t.closest('[data-dl-hold]')) hold();
    else if (restore) run(async () => report(await call('devleft_restore', { ids: [restore.dataset.dlRestore] }), 'Restored'));
    else if (del && held(del.dataset.dlDelete)) deleteNow(held(del.dataset.dlDelete));
  });
  render();
}

/** Scan again (on first show and from the panel's refresh button). */
export function showDevLeft() {
  if (!dl.busy) refresh();
}
