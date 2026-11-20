// Orbit / palette: Ctrl+K command palette. Jump to a view, run an action, or act on any
// running process by typing ("end chrome", "theme void", "performance").

import { call } from './api.js';
import { store, on, setSettings, setQuery, selectProcess } from './store.js';
import { fuzzy, escapeHtml, pct, bytes } from './format.js';
import { navigate, toast, restartAsAdmin, openRunTask, checkForUpdates } from './app.js';
import { endTask, exportCsv, restartProcess } from './views/processes.js';
import { layoutVariants } from './layouts.js';

const MAX_ACTIONS = 10;
const MAX_PROCESS_ITEMS = 24;
const IDLE_PROCESSES = 5;
// Fewer matches than this: also try the query as typed on the PC's other keyboard layouts.
const FEW_MATCHES = 3;
const FILE_MIN_CHARS = 2;
const FILE_DEBOUNCE_MS = 120;
const INDEX_POLL_MS = 500;

let root = null;
let input = null;
let list = null;
let open = false;
let results = [];
let active = 0;
let returnFocus = null;
let renderQueued = false;

// File search (v1.4): results arrive from Rust after the rest of the list.
let layoutMaps = null; // keyboard_layouts reply, fetched once
let fileQuery = '';   // the palette text the file state below belongs to
let fileItems = [];
let fileStatus = null;
let fileTimer = 0;
let fileSeq = 0;
// The query whose results are shown: not searched again (their ids stay the ones Rust holds).
let fileLoaded = null;

const icon = (id) => `<svg class="icon" aria-hidden="true"><use href="#${id}"/></svg>`;

// ---------------------------------------------------------------- item sources

const VIEWS = [
  ['processes', 'Processes', 'i-list', '01', 'tasks apps'],
  ['performance', 'Performance', 'i-chart', '02', 'cpu memory disk network gpu graphs'],
  ['history', 'History', 'i-history', '03', 'app history usage past days cpu time data'],
  ['startup', 'Startup', 'i-rocket', '04', 'boot autorun run keys scheduled tasks autoruns'],
  ['users', 'Users', 'i-users', '05', 'sessions accounts sign out disconnect'],
  ['services', 'Services', 'i-services', '06', 'windows services'],
  ['network', 'Network', 'i-plug', '07', 'connections ports sockets tcp udp internet'],
  ['security', 'Security', 'i-radar', '08', 'scan signatures malware suspicious signed'],
  ['optimize', 'Optimize', 'i-gauge', '09', 'health report pc check cleanup boost drivers speed up'],
  ['rig', 'My Rig', 'i-rig', '10', 'specs hardware system info cpu gpu graphics card ram memory motherboard bios drives displays windows version copy specs'],
  ['settings', 'Settings', 'i-gear', '11', 'preferences options themes ctrl comma'],
];

// Commands other modules add (docs/V04-CONTRACT.md "Palette registry").
const providers = [];

/**
 * Add palette items from another module. `provider()` is called each time the palette
 * renders and returns items in the built-in shape:
 * { label, search?, icon, hint?, danger?, run } (group is set to 'Actions').
 * They are listed after the built-in actions. Returns a function that removes the provider.
 */
export function registerCommands(provider) {
  if (typeof provider !== 'function') return () => {};
  providers.push(provider);
  return () => {
    const i = providers.indexOf(provider);
    if (i >= 0) providers.splice(i, 1);
  };
}

function registeredItems() {
  const out = [];
  for (const provider of providers) {
    let items;
    try {
      items = provider();
    } catch (err) {
      console.error('[orbit] palette provider failed', err);
      continue;
    }
    if (!Array.isArray(items)) continue;
    for (const it of items) {
      if (!it || typeof it.label !== 'string' || typeof it.run !== 'function') continue;
      out.push({ icon: 'i-spark', hint: '', search: it.label, ...it, group: 'Actions' });
    }
  }
  return out;
}

const THEMES = [
  ['night', 'Night'],
  ['void', 'Void'],
  ['signal', 'Signal'],
  ['daylight', 'Daylight'],
];

function gotoItems() {
  return VIEWS.map(([view, label, ic, index, extra]) => ({
    group: 'Go to',
    label,
    search: `${label} go to ${extra}`,
    icon: ic,
    hint: index,
    run: () => navigate(view),
  }));
}

function actionItems() {
  const s = store.settings;
  const items = [];
  items.push({
    group: 'Actions',
    label: 'Run new task',
    search: 'run new task open program start launch command cmd file run box',
    icon: 'i-play',
    hint: 'Ctrl+N',
    run: () => openRunTask(),
  });
  items.push({
    group: 'Actions',
    label: 'Always on top',
    search: 'always on top pin window float topmost keep above',
    icon: 'i-pin',
    hint: s.alwaysOnTop ? 'On' : 'Off',
    run: () => setSettings({ alwaysOnTop: !store.settings.alwaysOnTop }),
  });
  for (const [id, name] of THEMES) {
    items.push({
      group: 'Actions',
      label: `Theme: ${name}`,
      search: `theme ${name} colour color appearance`,
      icon: 'i-spark',
      hint: s.theme === id ? 'Current' : '',
      run: () => setSettings({ theme: id }),
    });
  }
  items.push({
    group: 'Actions',
    label: s.paused ? 'Resume updates' : 'Pause updates',
    search: s.paused ? 'resume updates live play unpause' : 'pause updates freeze stop live',
    icon: s.paused ? 'i-play' : 'i-pause',
    hint: 'Space',
    run: () => setSettings({ paused: !store.settings.paused }),
  });
  for (const ms of [500, 1000, 2000, 5000]) {
    items.push({
      group: 'Actions',
      label: `Update speed: ${(ms / 1000).toFixed(1)} s`,
      search: `update speed interval refresh rate ${ms / 1000}s ${(ms / 1000).toFixed(1)} s`,
      icon: 'i-clock',
      hint: s.intervalMs === ms ? 'Current' : '',
      run: () => setSettings({ intervalMs: ms }),
    });
  }
  items.push({
    group: 'Actions',
    label: s.compact ? 'Comfortable rows' : 'Compact rows',
    search: 'compact rows density dense comfortable table',
    icon: 'i-list',
    hint: s.compact ? 'On' : 'Off',
    run: () => setSettings({ compact: !store.settings.compact }),
  });
  items.push({
    group: 'Actions',
    label: s.reducedMotion ? 'Turn animations on' : 'Turn animations off',
    search: 'animations motion reduce effects hover accessibility',
    icon: 'i-bolt',
    hint: s.reducedMotion ? 'Off' : 'On',
    run: () => setSettings({ reducedMotion: !store.settings.reducedMotion }),
  });
  for (const [id, label, ic] of [['grouped', 'Group processes by type', 'i-grid'], ['tree', 'Show processes as a tree', 'i-tree'], ['flat', 'Show processes as a flat list', 'i-list']]) {
    items.push({
      group: 'Actions',
      label,
      search: `${label} view grouping ${id}`,
      icon: ic,
      hint: s.grouping === id ? 'Current' : '',
      run: () => {
        setSettings({ grouping: id });
        navigate('processes');
      },
    });
  }
  items.push({
    group: 'Actions',
    label: 'Export process list (CSV)',
    search: 'export csv report save download processes',
    icon: 'i-download',
    hint: '',
    run: () => exportCsv(),
  });
  if (!(store.staticInfo && store.staticInfo.isAdmin)) {
    items.push({
      group: 'Actions',
      label: 'Restart as administrator',
      search: 'restart as administrator admin elevate run as admin uac',
      icon: 'i-shield',
      hint: '',
      run: () => restartAsAdmin(),
    });
  }
  items.push({
    group: 'Actions',
    label: 'Check for updates',
    search: 'check for updates update upgrade new version',
    icon: 'i-refresh',
    hint: '',
    run: () => checkForUpdates({ manual: true }),
  });
  return items.concat(registeredItems());
}

// Open a process in the Processes view. The search is global, so an active query
// that does not match this process would hide its row: clear it first.
function showDetails(pid) {
  if ((store.query || '') !== '') {
    const search = document.getElementById('search');
    if (search && search.value) {
      search.value = '';
      search.dispatchEvent(new Event('input', { bubbles: true }));
    }
    setQuery('');
  }
  navigate('processes');
  selectProcess(pid);
}

// "end 6852", "6852 end", "6852": the PID and the leftover verb text ('' for digits only).
function splitPid(q) {
  let m = q.match(/^(.*?)\s*\b(\d+)$/);
  if (m) return { pid: Number(m[2]), verb: m[1].trim() };
  m = q.match(/^(\d+)\s+(.+)$/);
  if (m) return { pid: Number(m[1]), verb: m[2].trim() };
  return null;
}

function procHint(r) {
  const parts = [`PID ${r.pid}`];
  if (typeof r.cpu === 'number') parts.push(pct(r.cpu, 1));
  if (typeof r.memory === 'number') parts.push(bytes(r.memory));
  if (r.notResponding) parts.push('Not responding');
  else if (r.suspended) parts.push('Suspended');
  return parts.join(' / ');
}

function processCandidates(rows) {
  const out = [];
  for (let i = 0; i < rows.length; i++) {
    const r = rows[i];
    const hint = procHint(r);
    const extra = `${r.pid} ${r.windowTitle || ''}`;
    const base = { group: 'Processes', hint, row: r };
    out.push({
      ...base,
      label: `End ${r.name}`,
      search: `End ${r.name} kill stop ${extra}`,
      icon: 'i-stop',
      danger: true,
      run: () => endTask(r.pid, r.name, r.startTime),
    });
    out.push({
      ...base,
      label: `Restart ${r.name}`,
      search: `Restart ${r.name} relaunch reopen ${extra}`,
      icon: 'i-refresh',
      run: () => restartProcess(r.pid, r.name, r.startTime),
    });
    out.push({
      ...base,
      label: `Details ${r.name}`,
      search: `Details ${r.name} info inspect ${extra}`,
      icon: 'i-spark',
      run: () => showDetails(r.pid),
    });
    if (r.exe) {
      out.push({
        ...base,
        label: `Open location ${r.name}`,
        search: `Open location ${r.name} folder file path ${extra}`,
        icon: 'i-folder',
        run: () => call('open_location', { path: r.exe }).catch((err) => toast(String(err), 'error')),
      });
    }
  }
  return out;
}

// ---------------------------------------------------------------- matching

function score(item, q) {
  const a = fuzzy(item.label, q);
  const b = fuzzy(item.search, q);
  if (a < 0 && b < 0) return -1;
  // Label matches outrank keyword matches.
  return Math.max(a >= 0 ? a + 50 : -1, b);
}

// The views, actions and processes matching `q` (all of them for an empty query).
function matchGroups(q, rows) {
  const groups = [];
  if (!q) {
    groups.push(['Go to', gotoItems()]);
    groups.push(['Actions', actionItems().slice(0, MAX_ACTIONS)]);
    const top = [...rows].sort((a, b) => (b.cpu || 0) - (a.cpu || 0)).slice(0, IDLE_PROCESSES);
    groups.push(['Processes', top.map((r) => ({
      group: 'Processes',
      label: `Details ${r.name}`,
      hint: procHint(r),
      icon: 'i-spark',
      row: r,
      run: () => showDetails(r.pid),
    }))]);
  } else {
    const rank = (items, limit) => items
      .map((it) => ({ it, s: score(it, q) }))
      .filter((x) => x.s >= 0)
      .sort((x, y) => y.s - x.s || x.it.label.localeCompare(y.it.label))
      .slice(0, limit)
      .map((x) => x.it);

    groups.push(['Go to', rank(gotoItems(), VIEWS.length)]);
    groups.push(['Actions', rank(actionItems(), MAX_ACTIONS)]);

    const out = processCandidates(rows);
    let procs = null;
    // A PID typed (alone or with a verb such as "end 6852"): that process's actions first,
    // narrowed and ordered by the verb. A PID nobody has falls through to normal matching.
    const split = splitPid(q);
    if (split) {
      const hits = out
        .filter((it) => it.row.pid === split.pid)
        .map((it) => ({ it, s: split.verb ? fuzzy(it.label, split.verb) : 0 }))
        .filter((x) => x.s >= 0)
        .sort((x, y) => y.s - x.s)
        .map((x) => x.it);
      if (hits.length) procs = hits;
      else if (!split.verb) procs = rank(out, MAX_PROCESS_ITEMS);
    }
    if (!procs) {
      // Process actions only match on their label (name, verb) or PID / window title,
      // and prefer substring matches so "code" does not match every long command line.
      procs = out
        .map((it) => {
          const s = Math.max(fuzzy(it.label, q), fuzzy(it.search, q) >= 1000 ? fuzzy(it.search, q) - 100 : -1);
          const boost = it.row.kind === 'app' ? 40 : 0;
          return { it, s: s >= 0 ? s + boost + Math.min(30, it.row.cpu || 0) : -1 };
        })
        .filter((x) => x.s >= 0)
        .sort((x, y) => y.s - x.s)
        .slice(0, MAX_PROCESS_ITEMS)
        .map((x) => x.it);
    }
    groups.push(['Processes', procs]);
  }
  return groups;
}

const countItems = (groups) => groups.reduce((n, [, items]) => n + items.length, 0);

// The query re-typed on another layout, when the text as typed finds little.
function withLayouts(q, rows, groups) {
  if (!q || countItems(groups) >= FEW_MATCHES) return { groups, alt: null };
  let alt = null;
  for (const v of layoutVariants(q, layoutMaps)) {
    const extra = matchGroups(v, rows);
    if (!countItems(extra)) continue;
    alt = alt || v;
    for (const [name, items] of extra) {
      const g = groups.find(([n]) => n === name);
      if (!g) {
        groups.push([name, items]);
        continue;
      }
      for (const it of items) {
        if (!g[1].some((x) => x.label === it.label)) g[1].push(it);
      }
    }
  }
  return { groups, alt };
}

function fileGroup(q) {
  if (q !== fileQuery) return [];
  const items = [];
  if (fileStatus && fileStatus.state === 'unavailable') {
    items.push({
      group: 'Files',
      label: 'File search needs Windows Search, or Restart as administrator',
      icon: 'i-shield',
      hint: '',
      note: 'unavailable',
      run: () => restartAsAdmin(),
    });
  }
  return items.concat(fileItems);
}

function fileProgress(q) {
  if (q !== fileQuery || !fileStatus || fileStatus.state !== 'indexing') return '';
  const pct = typeof fileStatus.percent === 'number' ? ` ${fileStatus.percent}%` : '';
  return `Indexing ${fileStatus.drive || 'drives'}${pct}`;
}

function compute(keepActive = false) {
  const q = input ? input.value.trim() : '';
  const rows = (store.snapshot && Array.isArray(store.snapshot.processes)) ? store.snapshot.processes : [];
  const { groups, alt } = withLayouts(q, rows, matchGroups(q, rows));
  if (q) groups.push(['Files', fileGroup(q)]);
  if (!keepActive) scheduleFiles(q, alt);
  const prevActive = active;

  results = [];
  const html = [];
  for (const [name, items] of groups) {
    if (!items.length) continue;
    html.push(`<div class="palette-group micro" role="presentation">${escapeHtml(name)}</div>`);
    for (const it of items) {
      const i = results.length;
      results.push(it);
      html.push(
        `<div class="palette-item${it.danger ? ' palette-item--danger' : ''}" role="option" id="palette-opt-${i}" data-index="${i}" aria-selected="false">`
        + `${icon(it.icon)}<span class="palette-item__label">${highlight(it.label, q)}</span>`
        + (it.folder ? `<span class="palette-item__hint" title="${escapeHtml(it.folder)}">${escapeHtml(it.folder)}</span>` : '')
        + (it.hint ? `<span class="palette-item__hint">${escapeHtml(it.hint)}</span>` : '')
        + '</div>',
      );
    }
  }
  const progress = fileProgress(q);
  if (progress) html.push(`<div class="palette-note micro" role="status" data-palette-indexing>${escapeHtml(progress)}</div>`);
  if (!results.length && !progress) {
    html.push(`<div class="palette-empty">Nothing matches "${escapeHtml(q)}". Try a process name, a PID, a view or a file name.</div>`);
  }
  list.innerHTML = html.join('');
  setActive(results.length ? (keepActive && prevActive >= 0 && prevActive < results.length ? prevActive : 0) : -1, false);
}

function fileItem(r) {
  return {
    group: 'Files',
    label: r.name,
    folder: r.folder,
    icon: r.isDir ? 'i-folder' : 'i-file',
    run: () => call('file_search_open', { id: r.id, reveal: false }),
    reveal: () => call('file_search_open', { id: r.id, reveal: true }),
  };
}

// Ask Rust for files matching `q` (then each layout variant if nothing matched). Only the
// newest request's reply is used; Rust drops stale searches too.
function scheduleFiles(q, alt) {
  if (q === fileLoaded) return;
  clearTimeout(fileTimer);
  const seq = ++fileSeq;
  if (q !== fileQuery) {
    fileQuery = q;
    fileItems = [];
  }
  if ([...q].length < FILE_MIN_CHARS) {
    fileItems = [];
    return;
  }
  const tries = [q];
  if (alt) tries.push(alt);
  for (const v of layoutVariants(q, layoutMaps)) if (!tries.includes(v)) tries.push(v);
  fileTimer = setTimeout(async () => {
    for (const text of tries) {
      let reply;
      try {
        reply = await call('file_search', { query: text });
      } catch (err) {
        console.warn('[orbit] file search failed', err);
        return;
      }
      if (seq !== fileSeq || !open) return;
      if (!reply || reply.stale) return;
      fileStatus = reply.status || null;
      const found = Array.isArray(reply.results) ? reply.results : [];
      if (fileStatus && fileStatus.state === 'indexing') {
        fileItems = [];
        compute(true);
        fileTimer = setTimeout(() => { if (seq === fileSeq && open) scheduleFiles(q, alt); }, INDEX_POLL_MS);
        return;
      }
      if (found.length || text === tries[tries.length - 1]) {
        fileItems = found.map(fileItem);
        fileLoaded = q;
        compute(true);
        return;
      }
    }
  }, FILE_DEBOUNCE_MS);
}

function highlight(label, q) {
  if (!q) return escapeHtml(label);
  const idx = label.toLowerCase().indexOf(q.toLowerCase());
  if (idx === -1) return escapeHtml(label);
  return `${escapeHtml(label.slice(0, idx))}<mark>${escapeHtml(label.slice(idx, idx + q.length))}</mark>${escapeHtml(label.slice(idx + q.length))}`;
}

function setActive(i, scroll = true) {
  const prev = list.querySelector('.palette-item.is-active');
  if (prev) {
    prev.classList.remove('is-active');
    prev.setAttribute('aria-selected', 'false');
  }
  active = i;
  if (i < 0) {
    input.removeAttribute('aria-activedescendant');
    return;
  }
  const el = list.querySelector(`#palette-opt-${i}`);
  if (!el) return;
  el.classList.add('is-active');
  el.setAttribute('aria-selected', 'true');
  input.setAttribute('aria-activedescendant', el.id);
  if (scroll) el.scrollIntoView({ block: 'nearest' });
}

function runActive(i = active, reveal = false) {
  const it = results[i];
  if (!it) return;
  closePalette(false);
  try {
    const r = reveal && it.reveal ? it.reveal() : it.run();
    if (r && typeof r.catch === 'function') r.catch((err) => toast(String(err), 'error'));
  } catch (err) {
    toast(String(err && err.message ? err.message : err), 'error');
  }
}

// ---------------------------------------------------------------- open / close

function queueRender() {
  if (renderQueued) return;
  renderQueued = true;
  requestAnimationFrame(() => {
    renderQueued = false;
    if (open) compute();
  });
}

export function openPalette(initial) {
  if (!root) initPalette();
  if (open) {
    input.focus();
    input.select();
    return;
  }
  returnFocus = document.activeElement instanceof HTMLElement ? document.activeElement : null;
  open = true;
  root.classList.add('is-open');
  root.setAttribute('aria-hidden', 'false');
  input.setAttribute('aria-expanded', 'true');
  if (typeof initial === 'string') input.value = initial;
  // Fresh results each time it opens: the file index may have changed meanwhile.
  fileLoaded = null;
  fileQuery = '';
  fileItems = [];
  if (layoutMaps === null) {
    layoutMaps = [];
    call('keyboard_layouts')
      .then((maps) => {
        layoutMaps = Array.isArray(maps) ? maps : [];
        if (open) compute();
      })
      .catch(() => { layoutMaps = []; });
  }
  compute();
  input.focus();
  input.select();
}

function closePalette(restore = true) {
  if (!open) return;
  open = false;
  root.classList.remove('is-open');
  root.setAttribute('aria-hidden', 'true');
  input.setAttribute('aria-expanded', 'false');
  if (restore && returnFocus && document.contains(returnFocus)) {
    returnFocus.focus({ preventScroll: true });
  }
  returnFocus = null;
}

export function togglePalette() {
  if (open) closePalette();
  else openPalette();
}

export function isPaletteOpen() {
  return open;
}

export function initPalette() {
  if (root) return;
  root = document.getElementById('palette');
  if (!root) return;
  root.setAttribute('aria-hidden', 'true');
  root.innerHTML = `
    <div class="palette-box" role="dialog" aria-modal="true" aria-label="Command palette">
      <div class="palette-input-row">
        ${icon('i-search')}
        <input class="palette-input" id="palette-input" type="text" maxlength="255" autocomplete="off" spellcheck="false"
          role="combobox" aria-controls="palette-list" aria-expanded="false" aria-autocomplete="list"
          aria-label="Type a command, view or process" placeholder="Type a command, view or process">
        <kbd class="kbd" aria-hidden="true">Esc</kbd>
      </div>
      <div class="palette-list" id="palette-list" role="listbox" aria-label="Results"></div>
      <div class="palette-foot micro" aria-hidden="true">
        <span><kbd class="kbd">&uarr;</kbd><kbd class="kbd">&darr;</kbd> Move</span>
        <span><kbd class="kbd">Enter</kbd> Run</span>
        <span><kbd class="kbd">Ctrl</kbd><kbd class="kbd">Enter</kbd> Show in folder</span>
        <span><kbd class="kbd">Esc</kbd> Close</span>
      </div>
    </div>`;
  input = root.querySelector('.palette-input');
  list = root.querySelector('.palette-list');

  input.addEventListener('input', () => compute());

  // Capture phase: while open, the palette owns the keyboard.
  window.addEventListener('keydown', (e) => {
    if (!open) return;
    const n = results.length;
    switch (e.key) {
      case 'Escape':
        e.preventDefault();
        e.stopImmediatePropagation();
        closePalette();
        return;
      case 'ArrowDown':
        e.preventDefault();
        e.stopImmediatePropagation();
        if (n) setActive((active + 1) % n);
        return;
      case 'ArrowUp':
        e.preventDefault();
        e.stopImmediatePropagation();
        if (n) setActive((active - 1 + n) % n);
        return;
      case 'PageDown':
        e.preventDefault();
        e.stopImmediatePropagation();
        if (n) setActive(Math.min(n - 1, active + 8));
        return;
      case 'PageUp':
        e.preventDefault();
        e.stopImmediatePropagation();
        if (n) setActive(Math.max(0, active - 8));
        return;
      case 'Enter':
        e.preventDefault();
        e.stopImmediatePropagation();
        runActive(active, e.ctrlKey || e.metaKey);
        return;
      case 'Tab':
        // Keep focus inside the palette.
        e.preventDefault();
        e.stopImmediatePropagation();
        input.focus();
        return;
      default:
        if ((e.ctrlKey || e.metaKey) && (e.key === 'k' || e.key === 'K')) {
          e.preventDefault();
          e.stopImmediatePropagation();
          closePalette();
          return;
        }
        // Everything else (typing) goes to the input; stop global shortcuts.
        e.stopImmediatePropagation();
    }
  }, true);

  list.addEventListener('mousemove', (e) => {
    const el = e.target.closest('.palette-item');
    if (!el) return;
    const i = Number(el.dataset.index);
    if (i !== active) setActive(i, false);
  });

  list.addEventListener('click', (e) => {
    const el = e.target.closest('.palette-item');
    if (!el) return;
    runActive(Number(el.dataset.index));
  });

  // Click outside the box closes.
  root.addEventListener('pointerdown', (e) => {
    if (e.target === root) {
      e.preventDefault();
      closePalette();
    }
  });

  // Settings can change from elsewhere while the palette is open (rare): refresh labels.
  on('settings', () => { if (open) queueRender(); });
}
