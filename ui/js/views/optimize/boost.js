// Optimize > Boost (0.5, feature 7d): game and focus boost.
// "Boost now" (or auto mode, when a listed game starts) lowers the listed background apps
// to Below normal, or pauses them, and switches the power plan to Ultimate / High
// performance. Turning it off puts everything back. Rust does the work (boost.rs); this
// tab edits the lists and toggles (settings boostGames, boostApps, boostAuto, boostPause,
// boostPower), sends them with boost_configure and shows the status from
// orbit://boost. Sub-tab API (see ../optimize.js): mount(paneEl, ctx), onShow(), onHide().
import { call, onEvent } from '../../api.js';
import { store, setSettings } from '../../store.js';
import { escapeHtml, bytes, time, pct } from '../../format.js';
import { toast, confirmDialog } from '../../app.js';

const PRIORITY = { idle: 'Low', belowNormal: 'Below normal', normal: 'Normal', aboveNormal: 'Above normal', high: 'High', realtime: 'Realtime' };
const PAUSE_WARNING = 'Paused apps stop completely until Boost turns off: music stops, downloads and calls drop, and their windows do not respond. Only pause apps you will not need while you play.';

let root = null;
let ctx = null;
let status = null;
let suggestions = [];
let busy = false;
let dismissedRecovery = false;
const els = {};

const icon = (id) => `<svg class="icon" aria-hidden="true"><use href="#${id}"/></svg>`;
const text = (s) => escapeHtml(String(s ?? ''));
const plural = (n, one, many = `${one}s`) => `${Number(n || 0).toLocaleString()} ${n === 1 ? one : many}`;
const same = (a, b) => String(a).toLowerCase() === String(b).toLowerCase();
const has = (list, name) => list.some((n) => same(n, name));

function errMsg(err) {
  if (typeof err === 'string') return err;
  if (err && typeof err.message === 'string') return err.message;
  return 'Something went wrong.';
}

function config() {
  const s = store.settings;
  return { games: s.boostGames, apps: s.boostApps, auto: s.boostAuto, pause: s.boostPause, power: s.boostPower };
}

async function pushConfig() {
  try {
    setStatus(await call('boost_configure', { config: config() }));
  } catch (err) {
    toast(errMsg(err), 'error');
  }
}

function save(patch) {
  setSettings(patch);
  render();
  pushConfig();
}

function setStatus(s) {
  if (!s) return;
  status = s;
  ctx?.setBadge(s.active ? 'On' : '', s.active ? 'warn' : '');
  if (root) render();
}

async function refresh() {
  try {
    setStatus(await call('boost_status'));
  } catch (err) {
    console.error('[orbit] boost_status failed', err);
  }
  try {
    suggestions = await call('boost_suggestions');
  } catch {
    suggestions = [];
  }
  if (root) render();
}

async function toggleBoost() {
  if (busy || !status) return;
  busy = true;
  render();
  try {
    if (status.active) {
      setStatus(await call('boost_stop'));
      const r = status.lastRestore;
      toast(r && r.failed.length ? `Boost is off. ${plural(r.failed.length, 'app')} could not be put back.` : 'Boost is off. Everything is back as it was.', r && r.failed.length ? 'warn' : 'success');
    } else {
      setStatus(await call('boost_start'));
      toast(`Boost is on. ${status.changed.length ? `${plural(status.changed.length, 'process', 'processes')} ${store.settings.boostPause ? 'paused' : 'lowered'}.` : 'No listed apps needed a change.'}`, 'success');
    }
  } catch (err) {
    toast(errMsg(err), 'error');
  } finally {
    busy = false;
  }
  render();
}

/** Running apps for the "Add" pickers: names once each, apps with windows first. */
function runningNames(exclude) {
  const rows = store.snapshot?.processes || [];
  const byName = new Map();
  for (const r of rows) {
    // Windows' own exes (explorer.exe has a window, so it is grouped as an app) never qualify.
    if (r.kind === 'windows' || /^[a-z]:\\windows\\/i.test(r.exe || '') || r.pid === store.staticInfo?.pid || has(exclude, r.name)) continue;
    const k = r.name.toLowerCase();
    const g = byName.get(k) || { name: r.name, app: false, title: '' };
    if (r.kind === 'app') {
      g.app = true;
      g.title = g.title || r.windowTitle || '';
    }
    byName.set(k, g);
  }
  return [...byName.values()].sort((a, b) => (b.app - a.app) || a.name.localeCompare(b.name));
}

function pickerHtml(kind, exclude) {
  const list = runningNames(exclude);
  const opt = (g) => `<option value="${text(g.name)}">${text(g.name)}${g.title ? ` — ${text(g.title.slice(0, 48))}` : ''}</option>`;
  const apps = list.filter((g) => g.app);
  const others = list.filter((g) => !g.app);
  return `<form class="bo-add" data-bo-add="${kind}">
      <select class="select bo-add-select" aria-label="Running app to add"${list.length ? '' : ' disabled'}>
        <option value="">${list.length ? 'Pick a running app' : 'No running apps to add'}</option>
        ${apps.length ? `<optgroup label="With a window">${apps.map(opt).join('')}</optgroup>` : ''}
        ${others.length ? `<optgroup label="Background">${others.map(opt).join('')}</optgroup>` : ''}
      </select>
      <button type="submit" class="btn btn--ghost btn--sm">Add</button>
      ${kind === 'games' ? `<button type="button" class="btn btn--ghost btn--sm" data-bo-browse>${icon('i-folder')}Browse</button>` : ''}
    </form>`;
}

function chipsHtml(kind, list, emptyText) {
  if (!list.length) return `<p class="bo-empty">${text(emptyText)}</p>`;
  const running = new Set((status?.runningGames || []).map((n) => n.toLowerCase()));
  return `<ul class="bo-names" aria-label="${kind === 'games' ? 'Games' : 'Apps to calm'}">${list.map((n) => `
      <li class="bo-name">
        <span class="bo-name-text mono">${text(n)}</span>
        ${kind === 'games' && running.has(n.toLowerCase()) ? '<span class="chip chip--sm chip--live">Running</span>' : ''}
        <button type="button" class="icon-btn bo-remove" data-bo-remove="${kind}" data-name="${text(n)}" aria-label="Remove ${text(n)}" title="Remove">${icon('i-x')}</button>
      </li>`).join('')}</ul>`;
}

function suggestionsHtml() {
  const s = store.settings;
  const list = suggestions.filter((x) => !has(s.boostApps, x.name) && !has(s.boostGames, x.name));
  if (!list.length) return '';
  return `<div class="bo-suggest">
      <p class="micro bo-sub-title">Busy right now</p>
      <ul class="bo-suggest-list">${list.map((x) => `
        <li class="bo-suggest-row">
          <span class="bo-suggest-name"><span class="mono">${text(x.name)}</span>${x.count > 1 ? `<span class="lv-faint"> ×${x.count}</span>` : ''}</span>
          <span class="bo-suggest-num" title="CPU now">${text(pct(x.cpu))}</span>
          <span class="bo-suggest-num" title="Memory">${text(bytes(x.memory))}</span>
          <button type="button" class="btn btn--ghost btn--sm" data-bo-suggest="${text(x.name)}">Add</button>
        </li>`).join('')}</ul>
    </div>`;
}

function switchHtml(key, labelId, on, disabled = false) {
  return `<button type="button" class="switch" role="switch" data-bo-switch="${key}" aria-checked="${on ? 'true' : 'false'}" aria-labelledby="${labelId}"${disabled ? ' disabled' : ''}><span class="switch-thumb" aria-hidden="true"></span></button>`;
}

/** "chrome.exe ×8" groups of the processes Boost changed. */
function changedGroups(changed) {
  const groups = new Map();
  for (const c of changed) {
    const k = `${c.name.toLowerCase()}|${c.action}|${c.before || ''}`;
    const g = groups.get(k) || { ...c, count: 0 };
    g.count++;
    groups.set(k, g);
  }
  return [...groups.values()];
}

function restoreLine(r) {
  const bits = [];
  bits.push(r.restored ? `put back ${plural(r.restored, 'process', 'processes')}` : 'had nothing to put back');
  if (r.exited) bits.push(`${plural(r.exited, 'process', 'processes')} had closed`);
  if (r.leftAlone) bits.push(`${plural(r.leftAlone, 'process', 'processes')} you changed were left as they were`);
  if (r.failed.length) bits.push(`${plural(r.failed.length, 'process', 'processes')} could not be put back (${r.failed.map((f) => f.name).slice(0, 3).join(', ')})`);
  const power = { restored: 'The power plan is back.', left: r.powerNote || '', failed: r.powerNote || '' }[r.power] || '';
  return `${bits.join(', ')}. ${power}`.trim();
}

function heroHtml() {
  const s = store.settings;
  const st = status;
  if (!st) return '<div class="bo-hero-text"><p class="bo-state">Reading Boost…</p></div>';
  const p = st.power;
  if (!st.active) {
    const what = [];
    if (s.boostApps.length) what.push(`${s.boostPause ? 'pauses' : 'lowers'} ${plural(s.boostApps.length, 'app')} on your list${s.boostPause ? '' : ' to Below normal'}`);
    if (s.boostPower && p.target && p.current?.guid !== p.target.guid) what.push(`switches to ${p.target.name}`);
    const planNote = s.boostPower && p.target && p.current?.guid === p.target.guid ? ` The power plan is already ${p.target.name}.` : '';
    const line = what.length ? `Boost ${what.join(' and ')}.`
      : s.boostPower ? `Add apps below so Boost has something to do.${planNote}`
      : 'Add apps below, or turn on the power plan switch, so Boost has something to do.';
    const auto = s.boostAuto && s.boostGames.length ? `<p class="bo-line">${icon('i-play')}Turns on by itself when ${text(s.boostGames.slice(0, 3).join(', '))}${s.boostGames.length > 3 ? ' or another listed game' : ''} starts.</p>` : '';
    const last = st.lastRestore ? `<p class="bo-line bo-line--muted">Last turned off at ${text(time(st.lastRestore.at))}: ${text(restoreLine(st.lastRestore))}</p>` : '';
    return `<div class="bo-hero-mark" aria-hidden="true">${icon('i-bolt')}</div>
      <div class="bo-hero-text">
        <p class="bo-state">Boost is off</p>
        <p class="bo-line">${text(line)}</p>
        ${auto}${last}
      </div>
      <button type="button" class="btn btn--primary bo-go" data-bo-toggle${busy ? ' disabled' : ''}>${icon('i-bolt')}Boost now</button>`;
  }
  const by = st.reason === 'auto' ? `Started when ${text(st.trigger || 'a game')} opened. Turns off when it closes.` : 'Started by you.';
  const groups = changedGroups(st.changed);
  const rows = groups.map((g) => `<li><span class="mono">${text(g.name)}</span>${g.count > 1 ? `<span class="lv-faint"> ×${g.count}</span>` : ''}
      <span class="bo-did">${g.action === 'paused' ? 'Paused' : `Below normal${g.before ? ` <span class="lv-faint">(was ${text(PRIORITY[g.before] || g.before)})</span>` : ''}`}</span></li>`).join('');
  const power = p.before ? `<li><span>Power plan</span><span class="bo-did">${text(p.current?.name || '')} <span class="lv-faint">(was ${text(p.before.name)})</span></span></li>` : '';
  const skipped = st.skipped.length ? `<details class="bo-skipped"><summary>${plural(st.skipped.length, 'process', 'processes')} left alone</summary>
      <ul>${st.skipped.map((n) => `<li><span class="mono">${text(n.name)}</span> <span class="lv-faint">${text(n.reason)}</span></li>`).join('')}</ul></details>` : '';
  return `<div class="bo-hero-mark is-on" aria-hidden="true">${icon('i-bolt')}</div>
    <div class="bo-hero-text">
      <p class="bo-state">Boost is on <span class="chip chip--sm chip--on">since ${text(time(st.since))}</span></p>
      <p class="bo-line">${by}</p>
      ${rows || power ? `<ul class="bo-did-list">${rows}${power}</ul>` : '<p class="bo-line bo-line--muted">Nothing needed a change: no listed app is running, and the power plan stayed.</p>'}
      ${p.note ? `<p class="bo-line bo-line--muted">${text(p.note)}</p>` : ''}
      ${skipped}
    </div>
    <button type="button" class="btn btn--primary bo-go" data-bo-toggle${busy ? ' disabled' : ''}>${icon('i-stop')}Turn off</button>`;
}

function render() {
  if (!root) return;
  const s = store.settings;
  els.hero.classList.toggle('is-on', !!status?.active);
  els.hero.innerHTML = heroHtml();

  const rec = status?.recovered;
  els.recovered.hidden = !rec || dismissedRecovery;
  if (rec && !dismissedRecovery) {
    els.recovered.querySelector('[data-bo-recovered-text]').textContent = `Orbit closed while Boost was on. When it started again it ${restoreLine(rec)}`;
  }

  els.apps.innerHTML = chipsHtml('apps', s.boostApps, 'No apps yet. Add the ones that slow your game down, like a browser, a launcher or a sync app.') + pickerHtml('apps', [...s.boostApps, ...s.boostGames]) + suggestionsHtml();
  els.games.innerHTML = chipsHtml('games', s.boostGames, 'No games yet. Pick one that is running, or browse to its .exe.') + pickerHtml('games', [...s.boostGames, ...s.boostApps]);

  for (const sw of root.querySelectorAll('[data-bo-switch]')) {
    const key = sw.dataset.boSwitch;
    sw.setAttribute('aria-checked', s[key] ? 'true' : 'false');
  }
  for (const r of root.querySelectorAll('[data-bo-mode]')) {
    const on = (r.dataset.boMode === 'pause') === !!s.boostPause;
    r.setAttribute('aria-checked', on ? 'true' : 'false');
    r.tabIndex = on ? 0 : -1;
  }
  els.pauseWarn.hidden = !s.boostPause;
  const p = status?.power;
  els.powerHelp.textContent = !p ? 'Reading power plans…'
    : p.before ? `${p.current?.name || p.target?.name || 'The new plan'} is on now. ${p.before.name} comes back when Boost turns off.`
      : !p.target ? (p.note || 'This PC has no High performance plan.')
      : p.current?.guid === p.target.guid ? `Already on ${p.target.name}, so there is nothing to switch.`
        : `Switches from ${p.current?.name || 'the current plan'} to ${p.target.name} while Boost is on.`;
  const editing = !!status?.active;
  els.activeNote.hidden = !editing;
}

function addName(kind, raw) {
  const name = String(raw || '').trim().split(/[\\/]/).pop().trim();
  if (!name) return;
  const key = kind === 'games' ? 'boostGames' : 'boostApps';
  const other = kind === 'games' ? 'boostApps' : 'boostGames';
  const s = store.settings;
  if (has(s[key], name)) return;
  const patch = { [key]: [...s[key], name] };
  // A game is never also an app to calm.
  if (has(s[other], name)) patch[other] = s[other].filter((n) => !same(n, name));
  save(patch);
}

function removeName(kind, name) {
  const key = kind === 'games' ? 'boostGames' : 'boostApps';
  save({ [key]: store.settings[key].filter((n) => !same(n, name)) });
}

async function setMode(pause) {
  if (pause === !!store.settings.boostPause) return;
  if (pause) {
    const ok = await confirmDialog({ title: 'Pause apps instead?', body: PAUSE_WARNING, confirmLabel: 'Pause them', danger: true });
    if (!ok) {
      render();
      return;
    }
  }
  save({ boostPause: pause });
}

async function browse() {
  try {
    const path = await call('boost_pick_exe');
    if (path) addName('games', path);
  } catch (err) {
    toast(errMsg(err), 'error');
  }
}

export function mount(paneEl, context) {
  root = paneEl;
  ctx = context;
  root.innerHTML = `
    <div class="toolbar bo-toolbar">
      <p class="bo-lede">Gives a game or a focused task more of the PC. Turning Boost off puts every app's priority and the power plan back exactly as they were. Windows' own processes and services are never touched.</p>
    </div>
    <div class="bo-recovered" data-bo-recovered hidden role="status">
      ${icon('i-history')}<p data-bo-recovered-text></p>
      <button type="button" class="icon-btn" data-bo-dismiss aria-label="Dismiss" title="Dismiss">${icon('i-x')}</button>
    </div>
    <section class="panel bo-hero" data-bo-hero aria-live="polite"></section>
    <p class="lv-help bo-active-note" data-bo-active-note hidden>Changes below apply the next time Boost turns on.</p>
    <div class="bo-grid">
      <section class="panel bo-panel" aria-labelledby="bo-apps-title">
        <div class="bo-panel-head">
          <h2 class="micro bo-panel-title" id="bo-apps-title">Apps to calm</h2>
          <button type="button" class="icon-btn" data-bo-refresh aria-label="Refresh suggestions" title="Refresh suggestions">${icon('i-refresh')}</button>
        </div>
        <div data-bo-apps></div>
      </section>
      <section class="panel bo-panel" aria-labelledby="bo-games-title">
        <div class="bo-panel-head">
          <h2 class="micro bo-panel-title" id="bo-games-title">Games</h2>
        </div>
        <div class="lv-field bo-field">
          <div class="lv-field-text">
            <span class="lv-label" id="bo-auto-l">Turn on when a game starts</span>
            <span class="lv-help">Boost turns on when one of these games opens and off when the last one closes.</span>
          </div>
          ${switchHtml('boostAuto', 'bo-auto-l', false)}
        </div>
        <div data-bo-games></div>
      </section>
    </div>
    <section class="panel bo-panel" aria-labelledby="bo-opts-title">
      <div class="bo-panel-head">
        <h2 class="micro bo-panel-title" id="bo-opts-title">While Boost is on</h2>
      </div>
      <div class="lv-field bo-field bo-field--wrap">
        <div class="lv-field-text">
          <span class="lv-label" id="bo-mode-l">Apps on the list</span>
          <span class="lv-help">Below normal lets them keep working, just behind your game.</span>
        </div>
        <div class="lv-seg" role="radiogroup" aria-labelledby="bo-mode-l">
          <button type="button" role="radio" data-bo-mode="lower" aria-checked="true">Below normal</button>
          <button type="button" role="radio" data-bo-mode="pause" aria-checked="false" tabindex="-1">Pause</button>
        </div>
      </div>
      <p class="bo-warn" data-bo-pause-warn hidden>${icon('i-pause')}<span>${text(PAUSE_WARNING)}</span></p>
      <div class="lv-field bo-field">
        <div class="lv-field-text">
          <span class="lv-label" id="bo-power-l">Switch the power plan</span>
          <span class="lv-help" data-bo-power-help></span>
        </div>
        ${switchHtml('boostPower', 'bo-power-l', true)}
      </div>
    </section>`;
  els.hero = root.querySelector('[data-bo-hero]');
  els.recovered = root.querySelector('[data-bo-recovered]');
  els.apps = root.querySelector('[data-bo-apps]');
  els.games = root.querySelector('[data-bo-games]');
  els.pauseWarn = root.querySelector('[data-bo-pause-warn]');
  els.powerHelp = root.querySelector('[data-bo-power-help]');
  els.activeNote = root.querySelector('[data-bo-active-note]');

  onEvent('orbit://boost', (s) => setStatus(s));

  root.addEventListener('click', (ev) => {
    const t = ev.target;
    if (t.closest('[data-bo-toggle]')) return void toggleBoost();
    const rm = t.closest('[data-bo-remove]');
    if (rm) return void removeName(rm.dataset.boRemove, rm.dataset.name);
    const sg = t.closest('[data-bo-suggest]');
    if (sg) return void addName('apps', sg.dataset.boSuggest);
    const sw = t.closest('[data-bo-switch]');
    if (sw && !sw.disabled) return void save({ [sw.dataset.boSwitch]: !store.settings[sw.dataset.boSwitch] });
    const mode = t.closest('[data-bo-mode]');
    if (mode) return void setMode(mode.dataset.boMode === 'pause');
    if (t.closest('[data-bo-browse]')) return void browse();
    if (t.closest('[data-bo-refresh]')) return void refresh();
    if (t.closest('[data-bo-dismiss]')) {
      dismissedRecovery = true;
      render();
    }
  });
  root.addEventListener('keydown', (ev) => {
    const mode = ev.target.closest('[data-bo-mode]');
    if (!mode || !['ArrowLeft', 'ArrowRight', 'ArrowUp', 'ArrowDown'].includes(ev.key)) return;
    ev.preventDefault();
    const pause = !store.settings.boostPause;
    setMode(pause).then(() => root.querySelector(`[data-bo-mode="${store.settings.boostPause ? 'pause' : 'lower'}"]`)?.focus());
  });
  root.addEventListener('submit', (ev) => {
    const form = ev.target.closest('[data-bo-add]');
    if (!form) return;
    ev.preventDefault();
    addName(form.dataset.boAdd, form.querySelector('select').value);
  });

  render();
  // The UI's settings are the source of truth; Rust keeps a copy for auto mode.
  pushConfig();
}

export function onShow() {
  refresh();
}

export function onHide() {}
