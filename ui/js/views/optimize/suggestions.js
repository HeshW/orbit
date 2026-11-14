// Startup suggestions (0.5, feature 7b): "These apps add the most to your sign-in. Turn them
// off?" Up to three enabled apps with the highest measured impact, from get_startup_suggestions
// (never Microsoft or security software). One click per app through the existing
// set_startup_enabled; nothing is ever turned off automatically. Used by the Startup view
// and by Optimize > Health.
import { call } from '../../api.js';
import { store, setSettings, on } from '../../store.js';
import { escapeHtml, bytes } from '../../format.js';
import { toast } from '../../app.js';

const IMPACT = { high: ['High', 'chip--danger'], medium: ['Medium', 'chip--warn'] };

function errMsg(err) {
  if (typeof err === 'string') return err;
  if (err && typeof err.message === 'string') return err.message;
  return 'Something went wrong.';
}

function attr(s) {
  return escapeHtml(String(s ?? '')).replace(/"/g, '&quot;');
}

/** A short, stable hash of the suggested apps (their ids, in any order). */
export function suggestionKey(list) {
  const ids = list.map((s) => String(s.id)).sort().join('|');
  let h = 0x811c9dc5;
  for (let i = 0; i < ids.length; i++) {
    h ^= ids.charCodeAt(i);
    h = Math.imul(h, 0x01000193) >>> 0;
  }
  return `s${list.length}-${h.toString(16)}`;
}

function seconds(ms) {
  return ms >= 10000 ? `${Math.round(ms / 1000)} s` : `${(ms / 1000).toFixed(1)} s`;
}

/**
 * Renders the suggestions card into `el` (hidden while there is nothing to suggest).
 * opts.onChange(): called after an app was turned off or back on, so the host can refresh.
 * opts.compact: leave out the explanation line (the Startup view, where room is short).
 * Returns { load(), isEmpty() }.
 */
export function createSuggestions(el, opts = {}) {
  let list = [];
  let loaded = false;
  const off = new Map(); // id -> suggestion turned off in this session (shown with Undo)
  const busy = new Set();

  function rowHtml(s) {
    const [label, chip] = IMPACT[s.impact] || ['Measured', ''];
    const isOff = off.has(s.id);
    const admin = s.needsAdmin && !store.staticInfo?.isAdmin;
    const cost = `${seconds(s.cpuMs)} of CPU and ${bytes(s.diskReadBytes)} read in its first minute`;
    const btn = isOff
      ? `<button type="button" class="btn btn--ghost btn--sm" data-sg="undo" data-id="${attr(s.id)}"${busy.has(s.id) ? ' disabled' : ''}>Undo</button>`
      : `<button type="button" class="btn btn--sm" data-sg="off" data-id="${attr(s.id)}"${busy.has(s.id) || admin ? ' disabled' : ''}
          ${admin ? 'title="Needs administrator rights. Try Restart as administrator."' : ''}>Turn off</button>`;
    return `<li class="sg-row${isOff ? ' is-off' : ''}">
      <div class="sg-text">
        <span class="sg-name">${escapeHtml(s.name)}</span>
        <span class="sg-cost">${isOff ? 'Turned off. It will not start with Windows.' : escapeHtml(cost)}${admin && !isOff ? '. Needs administrator rights.' : ''}</span>
      </div>
      ${isOff ? '' : `<span class="chip lv-chip ${chip}">${label}</span>`}
      ${btn}
    </li>`;
  }

  function render() {
    const rows = [...list];
    for (const s of off.values()) if (!rows.some((r) => r.id === s.id)) rows.push(s);
    // Hidden by the user for exactly this set of apps; a different set brings the card back.
    if (!loaded || !rows.length || (store.settings.hiddenSuggestions && store.settings.hiddenSuggestions === suggestionKey(list))) {
      el.hidden = true;
      el.innerHTML = '';
      return;
    }
    el.hidden = false;
    const n = list.length;
    const title = n === 0
      ? 'Done. Orbit has nothing else to suggest.'
      : `${n === 1 ? 'This app adds' : `These ${n} apps add`} the most to your sign-in. Turn ${n === 1 ? 'it' : 'them'} off?`;
    el.innerHTML = `
      <div class="sg-head">
        <svg class="icon" aria-hidden="true"><use href="#i-rocket"/></svg>
        <div class="sg-head-text">
          <p class="sg-title">${title}</p>
          ${opts.compact ? '' : `<p class="sg-help">Measured by Orbit at a sign-in. Turning an app off only stops it starting with Windows; you can still open it yourself. Orbit never turns anything off on its own and never suggests Windows or security apps.</p>`}
        </div>
        <button type="button" class="icon-btn sg-hide" data-sg-hide aria-label="Hide these suggestions" title="Hide until the suggestions change"><svg class="icon" aria-hidden="true"><use href="#i-close"/></svg></button>
      </div>
      <ul class="sg-list">${rows.map(rowHtml).join('')}</ul>`;
  }

  async function setEnabled(s, enabled) {
    busy.add(s.id);
    render();
    try {
      await call('set_startup_enabled', { id: s.id, enabled });
      if (enabled) {
        off.delete(s.id);
        toast(`${s.name} will start with Windows again.`, 'success');
      } else {
        off.set(s.id, s);
        toast(`${s.name} will no longer start with Windows.`, 'success');
      }
      busy.delete(s.id);
      await load();
      opts.onChange?.();
    } catch (err) {
      busy.delete(s.id);
      toast(errMsg(err), 'error');
      render();
    }
  }

  el.addEventListener('click', (ev) => {
    if (ev.target.closest('[data-sg-hide]')) {
      setSettings({ hiddenSuggestions: suggestionKey(list) });
      return;
    }
    const btn = ev.target.closest('[data-sg]');
    if (!btn || btn.disabled) return;
    const id = btn.dataset.id;
    const s = list.find((x) => x.id === id) || off.get(id);
    if (!s || busy.has(id)) return;
    setEnabled(s, btn.dataset.sg === 'undo');
  });

  async function load() {
    try {
      const r = await call('get_startup_suggestions');
      list = Array.isArray(r) ? r.filter((s) => !off.has(s.id)) : [];
    } catch {
      list = [];
    }
    loaded = true;
    render();
  }

  // Hidden here or in the other view (Startup / Health share the setting).
  on('settings', () => loaded && render());
  el.classList.add('sg');
  el.hidden = true;
  return { load, isEmpty: () => (!list.length && !off.size) || el.hidden };
}
