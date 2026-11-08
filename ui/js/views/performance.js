// 02 / Performance: live resource tiles on the left, the selected resource in detail
// on the right (big 60-sample chart, facts, top consumers).
//
// Rendering rules:
// - Nothing is drawn while the view is hidden; onShow redraws from store.history at once.
// - All DOM is built once. Per tick we only change text that actually changed and
//   redraw canvases. History series are copied into reused buffers (no per-tick
//   allocation for chart data).

import { store, on, setSettings, selectProcess } from '../store.js';
import * as fmt from '../format.js';
import { LineChart, sparkline } from '../charts.js';
import { toast } from '../app.js';
import { call } from '../api.js';

const INTERVALS = [500, 1000, 2000, 5000];
const ORDER = ['cpu', 'memory', 'disk', 'network', 'gpu'];
const STORAGE_KEY = 'orbit.perf.resource';
const IFACE_SAMPLES = 60;
const TOP_N = 5;
const ALT_SERIES = '--ink-2'; // second series (write / send): neutral, distinct from every accent

const RES = {
  cpu: { label: 'CPU', icon: 'i-cpu', color: '--chart-cpu', metric: 'CPU' },
  memory: { label: 'Memory', icon: 'i-memory', color: '--chart-mem', metric: 'memory' },
  disk: { label: 'Disk', icon: 'i-disk', color: '--chart-disk', metric: 'disk' },
  network: { label: 'Network', icon: 'i-network', color: '--chart-net', metric: 'network' },
  gpu: { label: 'GPU', icon: 'i-gpu', color: '--chart-gpu', metric: 'GPU' },
};

const FACTS = {
  cpu: [
    ['brand', 'Processor', true],
    ['speed', 'Speed'],
    ['logical', 'Logical processors'],
    ['physical', 'Cores'],
    ['procs', 'Processes'],
    ['threads', 'Threads'],
    ['handles', 'Handles'],
    ['uptime', 'Up time'],
  ],
  memory: [
    ['used', 'In use'],
    ['avail', 'Available'],
    ['commit', 'Committed / limit'],
    ['cached', 'Cached'],
    ['swap', 'Swap used / total'],
    ['total', 'Installed'],
  ],
  disk: [
    ['read', 'Read'],
    ['write', 'Write'],
    ['active', 'Active time'],
    ['volumes', 'Volumes'],
  ],
  network: [
    ['rx', 'Receive'],
    ['tx', 'Send'],
    ['totalRx', 'Received since boot'],
    ['totalTx', 'Sent since boot'],
    ['ifaces', 'Active interfaces'],
  ],
  gpu: [
    ['usage', 'Utilisation'],
    ['vram', 'Dedicated memory in use'],
    ['peak', 'Peak in window'],
    ['avg', 'Average in window'],
  ],
};

// ---------------------------------------------------------------- module state

let root = null;
let visible = false;
let selected = 'cpu';
let gpuSeen = false;
let rafId = 0;
let uptimeTimer = 0;
let snapAt = 0;
let staticApplied = null;
let staticFetched = null;
let staticFetching = false;

const head = {};          // header refs
const tiles = {};         // key -> { el, v, s, spark }
const panes = {};         // key -> { el, refs, facts, topRows }
const charts = {};        // LineChart instances
const buf = {
  cpu: [], mem: [], diskActive: [], diskRead: [], diskWrite: [], diskSum: [],
  rx: [], tx: [], net: [], gpu: [],
};
let cores = [];           // [{ cell, val, canvas, buf }]
const ifaceHist = new Map();  // name -> number[] (rx + tx, last 60)
let ifaceKey = '';
let ifaceRows = new Map();    // name -> { rx, tx, tot, spark }

const topP = new Array(TOP_N);
const topV = new Float64Array(TOP_N);
let topN = 0;

const iconCache = new Map();  // exe path -> data URL ('' = none / failed / pending)
const iconTries = new Map();  // exe path -> failed lookups (retried later, a few times)
const ICON_TRIES = 3;
const ICON_RETRY_MS = 4000;
const DASH = '–';

// ---------------------------------------------------------------- helpers

function setText(node, text) {
  if (!node) return;
  const s = text == null ? '' : String(text);
  if (node.__t !== s) {
    node.textContent = s;
    node.__t = s;
  }
}

function setHidden(node, hidden) {
  if (node && node.hidden !== hidden) node.hidden = hidden;
}

function collectRefs(container) {
  const out = {};
  container.querySelectorAll('[data-ref]').forEach((n) => { out[n.dataset.ref] = n; });
  return out;
}

// Copy a history ring buffer into a reused array, mapping null / NaN to 0.
function clean(src, dst, scale = 1) {
  const n = src ? src.length : 0;
  dst.length = n;
  for (let i = 0; i < n; i++) {
    const v = src[i];
    dst[i] = typeof v === 'number' && Number.isFinite(v) ? v * scale : 0;
  }
  return dst;
}

function maxOf(a, floor = 0) {
  let m = floor;
  for (let i = 0; i < a.length; i++) if (a[i] > m) m = a[i];
  return m;
}

function avgOf(a) {
  if (!a.length) return 0;
  let s = 0;
  for (let i = 0; i < a.length; i++) s += a[i];
  return s / a.length;
}

function ghz(mhz) {
  return mhz > 0 ? `${(mhz / 1000).toFixed(2)} GHz` : 'Not available';
}

function count(n) {
  return n == null ? 'Not available' : Number(n).toLocaleString();
}

function spanText(ms) {
  return fmt.historySpan(ms, true);
}

function intervalText(ms) {
  return `${(ms / 1000).toFixed(1)} s`;
}

function currentStatic() {
  return store.staticInfo || staticFetched;
}

function totalMemory(snap) {
  const si = currentStatic();
  return (snap && snap.memory && snap.memory.total) || (si && si.totalMemory) || 0;
}

function loadSelected() {
  try {
    const v = localStorage.getItem(STORAGE_KEY);
    if (v && RES[v]) return v;
  } catch (_) { /* storage blocked */ }
  return 'cpu';
}

function saveSelected(v) {
  try { localStorage.setItem(STORAGE_KEY, v); } catch (_) { /* storage blocked */ }
}

function schedule() {
  // Minimised: the store keeps its history; the visibilitychange listener redraws.
  if (!visible || rafId || document.hidden) return;
  rafId = requestAnimationFrame(render);
}

// ---------------------------------------------------------------- markup

function icon(id) {
  return `<svg class="icon" aria-hidden="true"><use href="#${id}"/></svg>`;
}

function tileHtml(key) {
  const r = RES[key];
  return `
    <button type="button" class="tile perf-tile perf-tile--${key}" role="tab" id="perf-tab-${key}"
      aria-controls="perf-pane-${key}" aria-selected="false" tabindex="-1" data-res="${key}"${key === 'gpu' ? ' hidden' : ''}>
      <span class="perf-tile__icon">${icon(r.icon)}</span>
      <span class="perf-tile__text">
        <span class="perf-tile__name">${r.label}</span>
        <span class="perf-tile__value" data-ref="v">–</span>
        <span class="perf-tile__sub" data-ref="s">Waiting for data</span>
      </span>
      <canvas class="perf-tile__spark" data-ref="spark" aria-hidden="true"></canvas>
    </button>`;
}

function paneHeadHtml(key, index, title) {
  return `
    <div class="perf-pane__head">
      <div class="perf-pane__id">
        <span class="perf-swatch perf-swatch--${key}" aria-hidden="true"></span>
        <div class="perf-pane__titles">
          <p class="micro">${index} / ${RES[key].label}</p>
          <h2 class="perf-pane__title" data-ref="title">${title}</h2>
        </div>
      </div>
      <div class="perf-readout">
        <span class="perf-readout__value" data-ref="headline">–</span>
        <span class="perf-readout__sub" data-ref="headSub"></span>
      </div>
    </div>`;
}

function chartHtml(ref, label, topLabel, opts = {}) {
  const legend = opts.legend || '';
  const size = opts.small ? ' perf-chart--small' : '';
  return `
    <div class="perf-chart-block">
      <div class="perf-chart-head">
        <p class="micro">${label} / <span class="js-span">60 seconds</span></p>
        ${legend}
      </div>
      <div class="perf-chart${size}">
        <span class="perf-chart__top" data-ref="${ref}Top">${topLabel}</span>
        <canvas class="perf-chart__canvas" data-ref="${ref}" role="img" aria-label="${label}, recent history"></canvas>
        ${opts.note ? `<p class="perf-chart__note" data-ref="${ref}Note" hidden>${opts.note}</p>` : ''}
      </div>
      <div class="perf-axis" aria-hidden="true"><span><span class="js-span">60 seconds</span> ago</span><span>Now</span></div>
    </div>`;
}

function legendHtml(items) {
  return `<div class="perf-legend">${items.map(([cls, label, ref]) => `
    <span class="perf-legend__item"><span class="perf-dot ${cls}" aria-hidden="true"></span>${label}
    <span class="perf-legend__val" data-ref="${ref}">–</span></span>`).join('')}</div>`;
}

function factsHtml(key) {
  return `
    <dl class="perf-facts">${FACTS[key].map(([k, label, wide]) => `
      <div class="perf-fact${wide ? ' perf-fact--wide' : ''}"><dt>${label}</dt><dd data-ref="f-${k}">–</dd></div>`).join('')}
    </dl>`;
}

function topHtml(key) {
  if (key === 'network') {
    return `
      <div class="perf-block">
        <div class="perf-block__head"><p class="micro">Top consumers / Network</p></div>
        <p class="perf-note">Per-app network use needs Windows event tracing, which Orbit does not
        collect yet. It is planned for a later version. Totals for each network interface are shown here.</p>
      </div>`;
  }
  const rows = Array.from({ length: TOP_N }, () => `
    <li hidden>
      <button type="button" class="perf-top__row" data-pid="">
        <span class="perf-top__badge" aria-hidden="true"><img alt="" hidden><span class="perf-top__initial"></span></span>
        <span class="perf-top__name"></span>
        <span class="perf-top__pid"></span>
        <span class="perf-top__value"></span>
        <span class="perf-top__bar" aria-hidden="true"><span class="perf-top__fill"></span></span>
      </button>
    </li>`).join('');
  return `
    <div class="perf-block">
      <div class="perf-block__head">
        <p class="micro"${key === 'disk' ? ` title="${fmt.IO_TITLE}"` : ''}>${key === 'disk' ? 'Top I/O / Processes' : `Top consumers / ${RES[key].label}`}</p>
        <span class="perf-block__hint" data-ref="topHint"></span>
      </div>
      <ol class="perf-top" data-ref="top">${rows}</ol>
      <p class="perf-note" data-ref="topEmpty" hidden></p>
    </div>`;
}

function lowerHtml(key) {
  return `
    <div class="perf-lower">
      <div class="perf-block">
        <div class="perf-block__head"><p class="micro">System facts / ${RES[key].label}</p></div>
        ${factsHtml(key)}
      </div>
      ${topHtml(key)}
    </div>`;
}

function paneHtml(key) {
  const idx = String(ORDER.indexOf(key) + 1).padStart(2, '0');
  let body = '';
  switch (key) {
    case 'cpu':
      body = `
        ${paneHeadHtml(key, idx, 'Processor')}
        ${chartHtml('chart', 'Utilisation', '100%')}
        <div class="perf-block perf-block--flush">
          <div class="perf-block__head">
            <p class="micro">Logical processors / <span data-ref="coreCount">0</span></p>
            <span class="perf-block__hint">Utilisation per core</span>
          </div>
          <div class="grid-cores perf-cores" data-ref="cores"></div>
        </div>
        ${lowerHtml(key)}`;
      break;
    case 'memory':
      body = `
        ${paneHeadHtml(key, idx, 'Memory')}
        ${chartHtml('chart', 'Memory in use', '100%')}
        <div class="perf-block perf-block--flush">
          <div class="perf-block__head"><p class="micro">Composition / Now</p></div>
          <div class="perf-comp" role="img" data-ref="comp" aria-label="Memory composition">
            <span class="perf-comp__seg perf-comp__seg--used" data-ref="segUsed"></span>
            <span class="perf-comp__seg perf-comp__seg--cached" data-ref="segCached"></span>
            <span class="perf-comp__seg perf-comp__seg--free" data-ref="segFree"></span>
          </div>
          <div class="perf-legend perf-legend--comp">
            <span class="perf-legend__item"><span class="perf-dot perf-dot--mem" aria-hidden="true"></span>In use <span class="perf-legend__val" data-ref="lUsed">–</span></span>
            <span class="perf-legend__item" data-ref="lCachedItem"><span class="perf-dot perf-dot--mem-cached" aria-hidden="true"></span>Cached <span class="perf-legend__val" data-ref="lCached">–</span></span>
            <span class="perf-legend__item"><span class="perf-dot perf-dot--free" aria-hidden="true"></span>Free <span class="perf-legend__val" data-ref="lFree">–</span></span>
          </div>
        </div>
        ${lowerHtml(key)}`;
      break;
    case 'disk':
      body = `
        ${paneHeadHtml(key, idx, 'Storage')}
        ${chartHtml('chart', 'Active time', '100%', { note: 'Active time is not available on this system.' })}
        ${chartHtml('rateChart', 'Transfer rate', 'Peak 0 B/s', {
          small: true,
          legend: legendHtml([['perf-dot--disk', 'Read', 'lRead'], ['perf-dot--alt', 'Write', 'lWrite']]),
        })}
        <div class="perf-block perf-block--flush">
          <div class="perf-block__head"><p class="micro">Volumes / Capacity</p></div>
          <ul class="perf-disks" data-ref="disks"></ul>
        </div>
        ${lowerHtml(key)}`;
      break;
    case 'network':
      body = `
        ${paneHeadHtml(key, idx, 'Network')}
        ${chartHtml('chart', 'Throughput', 'Peak 0 B/s', {
          legend: legendHtml([['perf-dot--net', 'Receive', 'lRx'], ['perf-dot--alt', 'Send', 'lTx']]),
        })}
        <div class="perf-block perf-block--flush">
          <div class="perf-block__head">
            <p class="micro">Interfaces / <span data-ref="ifaceCount">0</span></p>
            <span class="perf-block__hint">Busiest first</span>
          </div>
          <div class="perf-ifaces" role="table" aria-label="Network interfaces">
            <div class="perf-iface perf-iface--head" role="row">
              <span role="columnheader">Interface</span>
              <span role="columnheader" class="perf-num">Receive</span>
              <span role="columnheader" class="perf-num">Send</span>
              <span role="columnheader" class="perf-num">Total in / out</span>
              <span role="columnheader" class="perf-iface__trend">Trend</span>
            </div>
            <div data-ref="ifaces" role="rowgroup"></div>
          </div>
          <p class="perf-note" data-ref="ifaceEmpty" hidden>No network interfaces reported.</p>
        </div>
        ${lowerHtml(key)}`;
      break;
    case 'gpu':
      body = `
        ${paneHeadHtml(key, idx, 'Graphics')}
        ${chartHtml('chart', 'Utilisation', '100%', { note: 'No GPU counters in the latest sample.' })}
        ${lowerHtml(key)}`;
      break;
    default:
      break;
  }
  return `<section class="perf-pane perf-pane--${key}" id="perf-pane-${key}" role="tabpanel"
    aria-labelledby="perf-tab-${key}" data-res="${key}" hidden>${body}</section>`;
}

function viewHtml() {
  const seg = INTERVALS.map((ms) => `
    <button type="button" class="perf-seg__btn" data-ms="${ms}" aria-pressed="false">${ms < 1000 ? (ms / 1000).toFixed(1) : ms / 1000} s</button>`).join('');
  return `
    <header class="view-head perf-head">
      <div class="perf-head__text">
        <p class="micro micro--bright"><span class="signal" data-ref="signal"></span>02 / Performance</p>
        <h1 class="view-title">Every core. <em>In view.</em></h1>
      </div>
      <div class="toolbar perf-head__actions">
        <span class="chip chip--live" data-ref="live"><span data-ref="liveText">Live / 1.0 s</span></span>
        <div class="perf-seg" role="group" aria-label="Update speed" data-ref="seg">${seg}</div>
        <button type="button" class="btn btn--sm btn--ghost perf-pause" data-ref="pause">
          <svg class="icon" aria-hidden="true"><use data-ref="pauseIcon" href="#i-pause"/></svg>
          <span data-ref="pauseLabel">Pause</span>
        </button>
      </div>
    </header>
    <div class="perf-body">
      <div class="perf-tiles" role="tablist" aria-orientation="vertical" aria-label="Resources" data-ref="tiles">
        ${ORDER.map(tileHtml).join('')}
      </div>
      <div class="panel perf-detail" data-ref="detail">
        ${ORDER.map(paneHtml).join('')}
      </div>
    </div>`;
}

// ---------------------------------------------------------------- mount

export function mount(rootEl) {
  root = rootEl;
  root.classList.add('perf');
  root.innerHTML = viewHtml();

  Object.assign(head, collectRefs(root.querySelector('.perf-head')));

  for (const key of ORDER) {
    const el = root.querySelector(`#perf-tab-${key}`);
    tiles[key] = { el, ...collectRefs(el) };
    const pane = root.querySelector(`#perf-pane-${key}`);
    const refs = collectRefs(pane);
    const topRows = [];
    if (refs.top) {
      refs.top.querySelectorAll('li').forEach((li) => {
        const btn = li.querySelector('button');
        topRows.push({
          li, btn,
          img: btn.querySelector('img'),
          initial: btn.querySelector('.perf-top__initial'),
          name: btn.querySelector('.perf-top__name'),
          pid: btn.querySelector('.perf-top__pid'),
          value: btn.querySelector('.perf-top__value'),
          fill: btn.querySelector('.perf-top__fill'),
          lastPid: -1,
          lastSrc: '',
          lastFrac: -1,
        });
      });
    }
    panes[key] = { el: pane, refs, topRows, spans: pane.querySelectorAll('.js-span') };
  }

  charts.cpu = new LineChart(panes.cpu.refs.chart, { series: [{ color: RES.cpu.color, fill: true }], max: 100, grid: true });
  charts.memory = new LineChart(panes.memory.refs.chart, { series: [{ color: RES.memory.color, fill: true }], max: 100, grid: true });
  charts.diskActive = new LineChart(panes.disk.refs.chart, { series: [{ color: RES.disk.color, fill: true }], max: 100, grid: true });
  charts.diskRate = new LineChart(panes.disk.refs.rateChart, {
    series: [{ color: RES.disk.color, fill: true }, { color: ALT_SERIES, fill: false }], max: 'auto', grid: true,
  });
  charts.network = new LineChart(panes.network.refs.chart, {
    series: [{ color: RES.network.color, fill: true }, { color: ALT_SERIES, fill: false }], max: 'auto', grid: true,
  });
  charts.gpu = new LineChart(panes.gpu.refs.chart, { series: [{ color: RES.gpu.color, fill: true }], max: 100, grid: true });

  bindEvents();

  if (store.snapshot && store.snapshot.gpu && store.snapshot.gpu.usage != null) {
    gpuSeen = true;
    setHidden(tiles.gpu.el, false);
  }
  if (store.snapshot) {
    snapAt = Date.now();
    trackInterfaces(store.snapshot);
  }

  syncSettings(store.settings || {});
  select(loadSelected(), false);

  on('snapshot', onSnapshotEvent);
  on('settings', (s) => {
    syncSettings(s || store.settings || {});
    schedule(); // theme may have changed: charts read CSS variables at draw time
  });
  on('query', () => schedule());
  document.addEventListener('visibilitychange', () => schedule());

  if (typeof ResizeObserver === 'function') {
    const ro = new ResizeObserver(() => schedule());
    ro.observe(root.querySelector('.perf-detail'));
    ro.observe(root.querySelector('.perf-tiles'));
  }
}

export function onShow() {
  visible = true;
  if (rafId) { cancelAnimationFrame(rafId); rafId = 0; }
  render();
  if (!uptimeTimer) uptimeTimer = setInterval(tickUptime, 1000);
}

export function onHide() {
  visible = false;
  if (rafId) { cancelAnimationFrame(rafId); rafId = 0; }
  if (uptimeTimer) { clearInterval(uptimeTimer); uptimeTimer = 0; }
}

// ---------------------------------------------------------------- events

function bindEvents() {
  const tilesEl = root.querySelector('.perf-tiles');

  tilesEl.addEventListener('click', (e) => {
    const t = e.target.closest('.perf-tile');
    if (t) select(t.dataset.res, false);
  });

  tilesEl.addEventListener('keydown', (e) => {
    if (!['ArrowDown', 'ArrowUp', 'ArrowLeft', 'ArrowRight', 'Home', 'End'].includes(e.key)) return;
    const list = ORDER.map((k) => tiles[k].el).filter((el) => !el.hidden);
    const cur = list.indexOf(e.target.closest('.perf-tile'));
    if (cur < 0) return;
    let next = cur;
    if (e.key === 'ArrowDown' || e.key === 'ArrowRight') next = (cur + 1) % list.length;
    else if (e.key === 'ArrowUp' || e.key === 'ArrowLeft') next = (cur - 1 + list.length) % list.length;
    else if (e.key === 'Home') next = 0;
    else if (e.key === 'End') next = list.length - 1;
    e.preventDefault();
    select(list[next].dataset.res, true);
  });

  head.seg.addEventListener('click', (e) => {
    const b = e.target.closest('.perf-seg__btn');
    if (!b) return;
    const intervalMs = Number(b.dataset.ms);
    if (!intervalMs || intervalMs === (store.settings && store.settings.intervalMs)) return;
    applySampling({ intervalMs });
  });

  head.pause.addEventListener('click', () => {
    applySampling({ paused: !(store.settings && store.settings.paused) });
  });

  root.querySelector('.perf-detail').addEventListener('click', (e) => {
    const row = e.target.closest('.perf-top__row');
    if (!row || !row.dataset.pid) return;
    selectProcess(Number(row.dataset.pid));
  });
}

async function applySampling(patch) {
  const s = store.settings || {};
  const next = {
    intervalMs: patch.intervalMs != null ? patch.intervalMs : s.intervalMs || 1000,
    paused: patch.paused != null ? patch.paused : !!s.paused,
  };
  setSettings(patch);
  try {
    await call('set_sampling', { intervalMs: next.intervalMs, paused: next.paused, delta: true });
    if (patch.paused != null) toast(next.paused ? 'Updates paused' : 'Updates resumed', 'info');
  } catch (err) {
    toast(typeof err === 'string' ? err : 'Could not change the update speed.', 'error');
  }
}

function syncSettings(s) {
  const ms = s.intervalMs || 1000;
  const paused = !!s.paused;
  head.seg.querySelectorAll('.perf-seg__btn').forEach((b) => {
    const on = Number(b.dataset.ms) === ms;
    b.setAttribute('aria-pressed', on ? 'true' : 'false');
    b.classList.toggle('is-on', on);
  });
  head.pauseIcon.setAttribute('href', paused ? '#i-play' : '#i-pause');
  setText(head.pauseLabel, paused ? 'Resume' : 'Pause');
  setText(head.liveText, paused ? `Paused / ${intervalText(ms)}` : `Live / ${intervalText(ms)}`);
  head.live.classList.toggle('chip--live', !paused);
  head.signal.classList.toggle('is-paused', paused);
  root.classList.toggle('is-paused', paused);
  const span = spanText(ms);
  for (const key of ORDER) panes[key].spans.forEach((n) => setText(n, span));
}

function select(res, focus) {
  if (!RES[res] || (res === 'gpu' && !gpuSeen)) res = 'cpu';
  selected = res;
  for (const key of ORDER) {
    const on = key === res;
    const t = tiles[key].el;
    t.setAttribute('aria-selected', on ? 'true' : 'false');
    t.tabIndex = on ? 0 : -1;
    t.classList.toggle('is-active', on);
    setHidden(panes[key].el, !on);
  }
  saveSelected(res);
  if (focus) tiles[res].el.focus();
  if (visible) {
    if (rafId) { cancelAnimationFrame(rafId); rafId = 0; }
    render();
  }
}

function onSnapshotEvent(snap) {
  snap = snap || store.snapshot;
  if (!snap) return;
  snapAt = Date.now();
  if (!gpuSeen && snap.gpu && snap.gpu.usage != null) {
    gpuSeen = true;
    setHidden(tiles.gpu.el, false);
  }
  trackInterfaces(snap);
  schedule();
}

function trackInterfaces(snap) {
  const list = (snap.network && snap.network.interfaces) || [];
  for (let i = 0; i < list.length; i++) {
    const it = list[i];
    let h = ifaceHist.get(it.name);
    if (!h) { h = []; ifaceHist.set(it.name, h); }
    if (h.length >= IFACE_SAMPLES) h.shift();
    h.push((it.rxBps || 0) + (it.txBps || 0));
  }
  if (ifaceHist.size > list.length) {
    for (const name of ifaceHist.keys()) {
      if (!list.some((it) => it.name === name)) ifaceHist.delete(name);
    }
  }
}

function tickUptime() {
  if (!visible || selected !== 'cpu') return;
  const snap = store.snapshot;
  if (!snap || !snap.system) return;
  const secs = snap.system.uptimeSecs + Math.max(0, (Date.now() - snapAt) / 1000);
  setText(panes.cpu.refs['f-uptime'], fmt.duration(Math.floor(secs)));
}

// ---------------------------------------------------------------- render

function render() {
  rafId = 0;
  if (!visible) return;
  const snap = store.snapshot;
  if (!snap) return;
  applyStatic();
  fillBuffers(snap);
  renderTiles(snap);
  switch (selected) {
    case 'cpu': renderCpu(snap); break;
    case 'memory': renderMemory(snap); break;
    case 'disk': renderDisk(snap); break;
    case 'network': renderNetwork(snap); break;
    case 'gpu': renderGpu(snap); break;
    default: break;
  }
}

function fillBuffers(snap) {
  const h = store.history || {};
  clean(h.cpu, buf.cpu);

  // history.mem may hold percent or bytes: normalise to percent of total.
  const memSrc = h.mem || [];
  const lastMem = memSrc.length ? memSrc[memSrc.length - 1] : 0;
  const total = totalMemory(snap);
  clean(memSrc, buf.mem, lastMem > 100.5 && total > 0 ? 100 / total : 1);

  clean(h.diskActive, buf.diskActive);
  clean(h.diskRead, buf.diskRead);
  clean(h.diskWrite, buf.diskWrite);
  const nd = Math.min(buf.diskRead.length, buf.diskWrite.length);
  buf.diskSum.length = nd;
  for (let i = 0; i < nd; i++) buf.diskSum[i] = buf.diskRead[i] + buf.diskWrite[i];

  clean(h.rx, buf.rx);
  clean(h.tx, buf.tx);
  const nn = Math.min(buf.rx.length, buf.tx.length);
  buf.net.length = nn;
  for (let i = 0; i < nn; i++) buf.net[i] = buf.rx[i] + buf.tx[i];

  clean(h.gpu, buf.gpu);
}

function memPct(m) {
  return m && m.total > 0 ? (m.used / m.total) * 100 : 0;
}

// Physical disk rates are null when the PDH counter is missing: show a dash, not "0 B/s".
function diskRate(v) {
  return v == null ? DASH : fmt.rate(v);
}

function diskSum(d) {
  return d.readBps == null || d.writeBps == null ? DASH : fmt.rate(d.readBps + d.writeBps);
}

function renderTiles(snap) {
  const { cpu, memory: m, disk, network: net, gpu } = snap;

  const tc = tiles.cpu;
  setText(tc.v, fmt.pct(cpu.usage, 0));
  setText(tc.s, cpu.freqMhz > 0 ? ghz(cpu.freqMhz) : `${(cpu.perCore || []).length} logical processors`);
  sparkline(tc.spark, buf.cpu, { color: RES.cpu.color, max: 100 });

  const tm = tiles.memory;
  setText(tm.v, fmt.pct(memPct(m), 0));
  setText(tm.s, `${fmt.bytes(m.used)} / ${fmt.bytes(m.total)}`);
  sparkline(tm.spark, buf.mem, { color: RES.memory.color, max: 100 });

  const td = tiles.disk;
  const rw = `R ${diskRate(disk.readBps)} · W ${diskRate(disk.writeBps)}`;
  if (disk.activePct != null) {
    setText(td.v, fmt.pct(disk.activePct, 0));
    setText(td.s, rw);
    sparkline(td.spark, buf.diskActive, { color: RES.disk.color, max: 100 });
  } else {
    setText(td.v, diskSum(disk));
    setText(td.s, rw);
    sparkline(td.spark, buf.diskSum, { color: RES.disk.color, max: maxOf(buf.diskSum, 1024) });
  }

  const tn = tiles.network;
  setText(tn.v, fmt.rate(net.rxBps + net.txBps));
  setText(tn.s, `R ${fmt.rate(net.rxBps)} · S ${fmt.rate(net.txBps)}`);
  sparkline(tn.spark, buf.net, { color: RES.network.color, max: maxOf(buf.net, 1024) });

  if (gpuSeen) {
    const tg = tiles.gpu;
    if (gpu && gpu.usage != null) {
      setText(tg.v, fmt.pct(gpu.usage, 0));
      setText(tg.s, gpu.dedicatedUsed != null ? `${fmt.bytes(gpu.dedicatedUsed)} dedicated` : 'Dedicated memory not reported');
    } else {
      setText(tg.v, 'No GPU counters');
      setText(tg.s, 'Counters not reported');
    }
    sparkline(tg.spark, buf.gpu, { color: RES.gpu.color, max: 100 });
  }
}

function setFact(P, key, value) {
  setText(P.refs[`f-${key}`], value);
}

// ---- CPU

function renderCpu(snap) {
  const P = panes.cpu;
  const { cpu, system } = snap;
  const si = currentStatic();
  const perCore = cpu.perCore || [];

  charts.cpu.draw([buf.cpu]);
  setText(P.refs.headline, fmt.pct(cpu.usage, 0));
  setText(P.refs.headSub, `${ghz(cpu.freqMhz)} · ${count(system.processCount)} processes`);

  renderCores(perCore);

  setFact(P, 'brand', (si && si.cpuBrand) || 'Not available');
  setFact(P, 'speed', ghz(cpu.freqMhz));
  setFact(P, 'logical', count((si && si.logicalCores) || perCore.length));
  setFact(P, 'physical', count(si ? si.physicalCores : null));
  setFact(P, 'procs', count(system.processCount));
  setFact(P, 'threads', count(system.threadCount));
  setFact(P, 'handles', count(system.handleCount));
  const secs = system.uptimeSecs + Math.max(0, (Date.now() - snapAt) / 1000);
  setFact(P, 'uptime', fmt.duration(Math.floor(secs)));

  renderTop(P, 'cpu', snap);
}

function renderCores(perCore) {
  const P = panes.cpu;
  const n = perCore.length;
  if (cores.length !== n) {
    let html = '';
    for (let i = 0; i < n; i++) {
      html += `<div class="perf-core heat">
        <div class="perf-core__head"><span class="perf-core__name">CPU ${i}</span><span class="perf-core__val">0%</span></div>
        <canvas class="perf-core__spark" aria-hidden="true"></canvas>
      </div>`;
    }
    P.refs.cores.innerHTML = html;
    cores = Array.from(P.refs.cores.children, (cell) => ({
      cell,
      val: cell.querySelector('.perf-core__val'),
      canvas: cell.querySelector('canvas'),
      buf: [],
      h: -1,
    }));
    setText(P.refs.coreCount, String(n));
  }
  const hist = (store.history && store.history.perCore) || [];
  for (let i = 0; i < n; i++) {
    const c = cores[i];
    const v = perCore[i] || 0;
    setText(c.val, fmt.pct(v, 0));
    const h = Math.round(Math.min(100, Math.max(0, v))) / 100;
    if (h !== c.h) { c.cell.style.setProperty('--h', h); c.h = h; }
    sparkline(c.canvas, clean(hist[i], c.buf), { color: RES.cpu.color, max: 100 });
  }
}

// ---- Memory

function renderMemory(snap) {
  const P = panes.memory;
  const m = snap.memory;
  const si = currentStatic();

  charts.memory.draw([buf.mem]);
  setText(P.refs.title, `${fmt.bytes(m.total || (si && si.totalMemory) || 0)} memory`);
  setText(P.refs.headline, fmt.bytes(m.used));
  setText(P.refs.headSub, `of ${fmt.bytes(m.total)} · ${fmt.pct(memPct(m), 0)} in use`);

  // Free = free + zeroed pages (memory.free), Cached = memory.cached, In use = the rest.
  // Without memory.free: in use = used, free = what is left after cached.
  const total = m.total || 1;
  const hasFree = m.free != null;
  const free = hasFree ? Math.min(total, Math.max(0, m.free)) : null;
  const cached = m.cached != null ? Math.min(Math.max(0, m.cached), hasFree ? total - free : total) : 0;
  const used = hasFree ? Math.max(0, total - free - cached) : Math.max(0, m.used || 0);
  const freeSeg = hasFree ? free : Math.max(0, total - used - cached);
  P.refs.segUsed.style.flexGrow = String(used / total);
  P.refs.segCached.style.flexGrow = String(cached / total);
  P.refs.segFree.style.flexGrow = String(freeSeg / total);
  setHidden(P.refs.segCached, m.cached == null);
  setHidden(P.refs.lCachedItem, m.cached == null);
  setText(P.refs.lUsed, fmt.bytes(used));
  setText(P.refs.lCached, fmt.bytes(cached));
  setText(P.refs.lFree, fmt.bytes(freeSeg));
  P.refs.comp.setAttribute('aria-label',
    `Memory composition: ${fmt.bytes(used)} in use${m.cached != null ? `, ${fmt.bytes(cached)} cached` : ''}, ${fmt.bytes(freeSeg)} free`);

  setFact(P, 'used', `${fmt.bytes(m.used)} (${fmt.pct(memPct(m), 0)})`);
  setFact(P, 'avail', fmt.bytes(m.available));
  setFact(P, 'commit', m.committed != null
    ? `${fmt.bytes(m.committed)} / ${m.commitLimit != null ? fmt.bytes(m.commitLimit) : 'Not available'}`
    : 'Not available');
  setFact(P, 'cached', m.cached != null ? fmt.bytes(m.cached) : 'Not available');
  setFact(P, 'swap', m.swapTotal > 0 ? `${fmt.bytes(m.swapUsed)} / ${fmt.bytes(m.swapTotal)}` : 'No swap file');
  setFact(P, 'total', fmt.bytes(m.total));

  renderTop(P, 'memory', snap);
}

// ---- Disk

function renderDisk(snap) {
  const P = panes.disk;
  const d = snap.disk;
  const hasActive = d.activePct != null;

  charts.diskActive.draw([buf.diskActive]);
  setHidden(P.refs.chartNote, hasActive);
  charts.diskRate.draw([buf.diskRead, buf.diskWrite]);
  setText(P.refs.rateChartTop, `Peak ${fmt.rate(Math.max(maxOf(buf.diskRead), maxOf(buf.diskWrite)))}`);
  setText(P.refs.lRead, diskRate(d.readBps));
  setText(P.refs.lWrite, diskRate(d.writeBps));

  setText(P.refs.headline, hasActive ? fmt.pct(d.activePct, 0) : diskSum(d));
  setText(P.refs.headSub, hasActive
    ? `active · R ${diskRate(d.readBps)} · W ${diskRate(d.writeBps)}`
    : `R ${diskRate(d.readBps)} · W ${diskRate(d.writeBps)}`);

  setFact(P, 'read', diskRate(d.readBps));
  setFact(P, 'write', diskRate(d.writeBps));
  setFact(P, 'active', hasActive ? fmt.pct(d.activePct, 1) : 'Not available');
  const si = currentStatic();
  setFact(P, 'volumes', count(si && si.disks ? si.disks.length : null));

  renderTop(P, 'disk', snap);
}

function renderDisks(si) {
  const P = panes.disk;
  const disks = (si && si.disks) || [];
  if (!disks.length) {
    P.refs.disks.innerHTML = '<li class="perf-note">No volumes reported.</li>';
    return;
  }
  P.refs.disks.innerHTML = disks.map((d) => {
    const used = Math.max(0, d.total - d.available);
    const frac = d.total > 0 ? used / d.total : 0;
    const kind = d.kind && d.kind !== 'Unknown' ? d.kind : '';
    const label = d.name && d.name !== d.mount ? d.name : 'Local disk';
    return `
      <li class="perf-disk">
        <div class="perf-disk__head">
          <span class="perf-disk__mount">${fmt.escapeHtml(d.mount)}</span>
          <span class="perf-disk__name">${fmt.escapeHtml(label)}</span>
          ${kind ? `<span class="chip perf-disk__chip">${fmt.escapeHtml(kind)}</span>` : ''}
          ${d.removable ? '<span class="chip perf-disk__chip">Removable</span>' : ''}
        </div>
        <div class="perf-bar${frac >= 0.9 ? ' perf-bar--warn' : ''}" role="img"
          aria-label="${fmt.escapeHtml(d.mount)} ${Math.round(frac * 100)} percent used">
          <span class="perf-bar__fill" style="transform:scaleX(${frac.toFixed(4)})"></span>
        </div>
        <div class="perf-disk__meta">
          <span>${fmt.bytes(used)} used of ${fmt.bytes(d.total)}</span>
          <span>${fmt.bytes(d.available)} free${d.fileSystem ? ` · ${fmt.escapeHtml(d.fileSystem)}` : ''}</span>
        </div>
      </li>`;
  }).join('');
}

// ---- Network

function renderNetwork(snap) {
  const P = panes.network;
  const n = snap.network;
  const list = n.interfaces || [];

  charts.network.draw([buf.rx, buf.tx]);
  setText(P.refs.chartTop, `Peak ${fmt.rate(Math.max(maxOf(buf.rx), maxOf(buf.tx)))}`);
  setText(P.refs.lRx, fmt.rate(n.rxBps));
  setText(P.refs.lTx, fmt.rate(n.txBps));

  setText(P.refs.headline, fmt.rate(n.rxBps));
  setText(P.refs.headSub, `received · sent ${fmt.rate(n.txBps)}`);

  let totRx = 0;
  let totTx = 0;
  let active = 0;
  for (let i = 0; i < list.length; i++) {
    totRx += list[i].totalRx || 0;
    totTx += list[i].totalTx || 0;
    if ((list[i].totalRx || 0) + (list[i].totalTx || 0) > 0) active++;
  }
  setFact(P, 'rx', fmt.rate(n.rxBps));
  setFact(P, 'tx', fmt.rate(n.txBps));
  setFact(P, 'totalRx', fmt.bytes(totRx));
  setFact(P, 'totalTx', fmt.bytes(totTx));
  setFact(P, 'ifaces', `${active} of ${list.length}`);

  renderInterfaces(list);
}

function renderInterfaces(list) {
  const P = panes.network;
  let key = '';
  for (let i = 0; i < list.length; i++) key += `${list[i].name}\n`;
  if (key !== ifaceKey) {
    ifaceKey = key;
    const sorted = list.slice().sort((a, b) =>
      ((b.totalRx || 0) + (b.totalTx || 0)) - ((a.totalRx || 0) + (a.totalTx || 0)) || a.name.localeCompare(b.name));
    P.refs.ifaces.innerHTML = sorted.map((it) => `
      <div class="perf-iface" role="row" data-name="${fmt.escapeHtml(it.name)}">
        <span class="perf-iface__name" role="cell" title="${fmt.escapeHtml(it.name)}">${fmt.escapeHtml(it.name)}</span>
        <span class="perf-num" role="cell" data-k="rx"></span>
        <span class="perf-num" role="cell" data-k="tx"></span>
        <span class="perf-num perf-iface__tot" role="cell" data-k="tot"></span>
        <span class="perf-iface__trend" role="cell"><canvas class="perf-iface__spark" aria-hidden="true"></canvas></span>
      </div>`).join('');
    ifaceRows = new Map();
    P.refs.ifaces.querySelectorAll('.perf-iface').forEach((row, i) => {
      ifaceRows.set(sorted[i].name, {
        rx: row.querySelector('[data-k="rx"]'),
        tx: row.querySelector('[data-k="tx"]'),
        tot: row.querySelector('[data-k="tot"]'),
        spark: row.querySelector('canvas'),
      });
    });
    setText(P.refs.ifaceCount, String(list.length));
    setHidden(P.refs.ifaceEmpty, list.length > 0);
  }
  for (let i = 0; i < list.length; i++) {
    const it = list[i];
    const r = ifaceRows.get(it.name);
    if (!r) continue;
    setText(r.rx, fmt.rate(it.rxBps));
    setText(r.tx, fmt.rate(it.txBps));
    setText(r.tot, `${fmt.bytes(it.totalRx)} / ${fmt.bytes(it.totalTx)}`);
    const h = ifaceHist.get(it.name);
    if (h) sparkline(r.spark, h, { color: RES.network.color, max: maxOf(h, 1024) });
  }
}

// ---- GPU

function renderGpu(snap) {
  const P = panes.gpu;
  const g = snap.gpu || {};
  const has = g.usage != null;

  charts.gpu.draw([buf.gpu]);
  setHidden(P.refs.chartNote, has);
  setText(P.refs.headline, has ? fmt.pct(g.usage, 0) : 'No GPU counters');
  setText(P.refs.headSub, g.dedicatedUsed != null ? `${fmt.bytes(g.dedicatedUsed)} dedicated memory` : 'Dedicated memory not reported');

  setFact(P, 'usage', has ? fmt.pct(g.usage, 1) : 'No GPU counters');
  setFact(P, 'vram', g.dedicatedUsed != null ? fmt.bytes(g.dedicatedUsed) : 'Not available');
  setFact(P, 'peak', fmt.pct(maxOf(buf.gpu), 1));
  setFact(P, 'avg', fmt.pct(avgOf(buf.gpu), 1));

  renderTop(P, 'gpu', snap);
}

// ---- Top consumers

function metricOf(key, p) {
  switch (key) {
    case 'cpu': return p.cpu;
    case 'memory': return p.memory;
    case 'disk': return (p.diskReadBps || 0) + (p.diskWriteBps || 0);
    case 'gpu': return p.gpu;
    default: return null;
  }
}

function pushTop(p, v) {
  if (topN === TOP_N && v <= topV[TOP_N - 1]) return;
  let i = topN < TOP_N ? topN++ : TOP_N - 1;
  while (i > 0 && topV[i - 1] < v) {
    topV[i] = topV[i - 1];
    topP[i] = topP[i - 1];
    i--;
  }
  topV[i] = v;
  topP[i] = p;
}

function matchesQuery(p, q) {
  if (String(p.pid) === q) return true;
  if (fmt.fuzzy(p.name, q) >= 0) return true;
  return !!(p.windowTitle && fmt.fuzzy(p.windowTitle, q) >= 0);
}

function formatMetric(key, v) {
  switch (key) {
    case 'cpu':
    case 'gpu': return fmt.pct(v, 1);
    case 'memory': return fmt.bytes(v);
    case 'disk': return fmt.rate(v);
    default: return '';
  }
}

function renderTop(P, key, snap) {
  const procs = snap.processes || [];
  const q = (store.query || '').trim();
  topN = 0;
  for (let i = 0; i < procs.length; i++) {
    const p = procs[i];
    if (p.pid === 0) continue; // System Idle Process is not a consumer
    const v = metricOf(key, p);
    if (v == null || !(v > 0)) continue;
    if (q && !matchesQuery(p, q)) continue;
    pushTop(p, v);
  }

  const total = key === 'memory' ? totalMemory(snap) : 0;
  const missing = [];
  for (let i = 0; i < P.topRows.length; i++) {
    const r = P.topRows[i];
    if (i >= topN) {
      setHidden(r.li, true);
      topP[i] = undefined;
      continue;
    }
    const p = topP[i];
    const v = topV[i];
    setHidden(r.li, false);
    if (r.lastPid !== p.pid) {
      r.btn.dataset.pid = String(p.pid);
      r.btn.setAttribute('aria-label', `${p.name}, PID ${p.pid}. Open details`);
      r.lastPid = p.pid;
    }
    setText(r.name, p.name);
    setText(r.pid, `PID ${p.pid}`);
    setText(r.value, formatMetric(key, v));
    setText(r.initial, (p.name || '?').charAt(0).toUpperCase());

    let frac;
    if (key === 'cpu' || key === 'gpu') frac = v / 100;
    else if (key === 'memory') frac = total > 0 ? v / total : 0;
    else frac = topV[0] > 0 ? v / topV[0] : 0;
    frac = Math.round(Math.min(1, Math.max(0, frac)) * 1000) / 1000;
    if (frac !== r.lastFrac) { r.fill.style.transform = `scaleX(${frac})`; r.lastFrac = frac; }

    const src = p.exe ? iconCache.get(p.exe) : '';
    if (p.exe && src === undefined) missing.push(p.exe);
    const want = src || '';
    if (want !== r.lastSrc) {
      r.lastSrc = want;
      if (want) r.img.src = want; else r.img.removeAttribute('src');
      setHidden(r.img, !want);
      setHidden(r.initial, !!want);
    }
  }
  for (let i = topN; i < TOP_N; i++) topP[i] = undefined;

  if (P.refs.topHint) setText(P.refs.topHint, q ? `Matching "${q}"` : key === 'disk' ? 'By I/O rate' : `By ${RES[key].metric} use`);
  if (P.refs.topEmpty) {
    const empty = topN === 0;
    setHidden(P.refs.topEmpty, !empty);
    if (empty) {
      setText(P.refs.topEmpty, q
        ? `No process matching "${q}" is using ${RES[key].metric} right now.`
        : (key === 'gpu' ? 'No per-process GPU use reported right now.' : `No process is using ${RES[key].metric} right now.`));
    }
  }

  if (missing.length) requestIcons(missing);
}

function requestIcons(paths) {
  for (const p of paths) iconCache.set(p, ''); // mark pending so we ask once
  call('get_icons', { paths }).then((map) => {
    let changed = false;
    for (const p of paths) {
      const src = map && typeof map === 'object' ? map[p] : undefined;
      if (typeof src === 'string' && src) { iconCache.set(p, src); changed = true; }
      else retryIcon(p);
    }
    if (changed) schedule();
  }).catch(() => { for (const p of paths) retryIcon(p); }); // the initial badge stays meanwhile
}

// Icon lookups can fail for a while after Orbit starts: forget the miss so a later render asks again.
function retryIcon(path) {
  const n = (iconTries.get(path) || 0) + 1;
  iconTries.set(path, n);
  if (n < ICON_TRIES) setTimeout(() => { if (iconCache.get(path) === '') iconCache.delete(path); }, ICON_RETRY_MS * n);
}

// ---- Static info (CPU brand, volumes)

function applyStatic() {
  const si = currentStatic();
  if (!si) {
    if (!staticFetching) {
      staticFetching = true;
      call('get_static_info').then((r) => { staticFetched = r; schedule(); }).catch(() => {});
    }
    return;
  }
  if (si === staticApplied) return;
  staticApplied = si;
  setText(panes.cpu.refs.title, si.cpuBrand || 'Processor');
  const disks = si.disks || [];
  setText(panes.disk.refs.title, disks.length === 1 ? '1 volume' : `${disks.length} volumes`);
  renderDisks(si);
}
