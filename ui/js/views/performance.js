// 02 / Performance: live resource tiles on the left, the selected resource in detail
// on the right (big 60-sample chart, facts, top consumers).
//
// Tiles: CPU, Memory, one per physical disk (Snapshot.disks; a single Disk tile when the
// list is empty), one per network interface that has carried traffic (plus a total when
// there are two or more), GPU (once counters were seen) and Battery (while
// Snapshot.battery is not null). Several tiles of one kind share that kind's detail pane,
// which shows the selected disk or interface.
//
// Rendering rules:
// - Nothing is drawn while the view is hidden; onShow redraws from history at once.
//   Per-disk, per-interface and battery history is kept here (the store only keeps the
//   totals) and is recorded on every snapshot, visible or not.
// - The tile list is rebuilt only when the set of disks / interfaces / battery changes.
//   Panes are built once. Per tick we only change text that actually changed and redraw
//   canvases.

import { store, on, setSettings, selectProcess, HISTORY_LEN } from '../store.js';
import * as fmt from '../format.js';
import { LineChart, sparkline } from '../charts.js';
import { toast } from '../app.js';
import { call } from '../api.js';

const INTERVALS = [500, 1000, 2000, 5000];
const ORDER = ['cpu', 'memory', 'disk', 'network', 'gpu', 'battery'];
const STORAGE_KEY = 'orbit.perf.resource';
const SAMPLES = HISTORY_LEN || 60;
const TOP_N = 5;
const ALT_SERIES = '--ink-2'; // second series (write / send): neutral, distinct from every accent
const BATTERY_STEP_MS = 60 * 1000; // one battery point a minute
const BATTERY_POINTS = 120;        // two hours
const MANY_TILES = 6;              // more than this: the tile column stops being sticky

const RES = {
  cpu: { label: 'CPU', icon: 'i-cpu', color: '--chart-cpu', metric: 'CPU' },
  memory: { label: 'Memory', icon: 'i-memory', color: '--chart-mem', metric: 'memory' },
  disk: { label: 'Disk', icon: 'i-disk', color: '--chart-disk', metric: 'disk' },
  network: { label: 'Network', icon: 'i-network', color: '--chart-net', metric: 'network' },
  gpu: { label: 'GPU', icon: 'i-gpu', color: '--chart-gpu', metric: 'GPU' },
  battery: { label: 'Battery', icon: 'i-battery', color: '--ok', metric: 'battery' },
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
    ['engine', 'Busiest engine'],
    ['peak', 'Peak in window'],
    ['avg', 'Average in window'],
  ],
  battery: [
    ['charge', 'Charge'],
    ['status', 'Status'],
    ['left', 'Time left'],
    ['source', 'Power source'],
  ],
};

// ---------------------------------------------------------------- module state

let root = null;
let tilesEl = null;
let visible = false;
let wanted = 'cpu';       // the tile the user picked (kept even while it is missing)
let selected = 'cpu';     // the tile shown now: wanted, or the first tile of its kind
let gpuSeen = false;
let rafId = 0;
let uptimeTimer = 0;
let snapAt = 0;
let staticApplied = null;
let staticFetched = null;
let staticFetching = false;

const head = {};          // header refs
let tileList = [];        // [{ key, kind, name, label, tag, el, v, s, spark }]
let tileMap = new Map();  // key -> tile
let layoutKey = null;
const panes = {};         // kind -> { el, refs, topRows, spans }
const charts = {};        // LineChart instances
const buf = {
  cpu: [], mem: [], diskActive: [], diskRead: [], diskWrite: [], diskSum: [],
  rx: [], tx: [], net: [], gpu: [],
};
let cores = [];           // [{ cell, val, canvas, buf }]
const diskHist = new Map();   // disk name -> { active, read, write, sum } (last 60 each)
const ifaceHist = new Map();  // interface name -> { rx, tx, sum } (last 60 each)
let ifaceKey = '';
let ifaceRows = new Map();    // name -> { row, rx, tx, tot, spark }
let ifaceSel = null;
let engineKey = null;
let engineRows = [];          // [{ val, fill, last }]
const batteryHist = [];       // percent, one point a minute, last one live
let batteryAt = 0;
let volumesFor = null;        // [staticInfo, letters] the volume list was built for

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

function num(v) {
  return typeof v === 'number' && Number.isFinite(v) ? v : 0;
}

// Push onto a fixed-length ring (null / NaN count as 0).
function ring(arr, v, len = SAMPLES) {
  if (arr.length >= len) arr.splice(0, arr.length - len + 1);
  arr.push(num(v));
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

function kindOf(key) {
  if (key.startsWith('disk:')) return 'disk';
  if (key.startsWith('net:')) return 'network';
  return RES[key] ? key : 'cpu';
}

function loadSelected() {
  try {
    const v = localStorage.getItem(STORAGE_KEY);
    if (typeof v === 'string' && v) return v;
  } catch (_) { /* storage blocked */ }
  return 'cpu';
}

function saveSelected(v) {
  try { localStorage.setItem(STORAGE_KEY, v); } catch (_) { /* storage blocked */ }
}

function schedule() {
  // Minimised: history keeps being recorded; the visibilitychange listener redraws.
  if (!visible || rafId || document.hidden) return;
  rafId = requestAnimationFrame(render);
}

// PDH PhysicalDisk instance "0 C: E:" -> { label: "Disk 0", letters: ["C:", "E:"] }.
function diskParts(name) {
  const m = /^(\d+)\s*(.*)$/.exec(String(name || '').trim());
  if (!m) return { label: String(name || 'Disk'), letters: [] };
  const letters = m[2].split(/\s+/).filter((s) => /^[A-Za-z]:$/.test(s)).map((s) => s.toUpperCase());
  return { label: `Disk ${m[1]}`, letters };
}

// Interfaces that carried traffic since boot, by name (a stable order for the tiles).
function activeIfaces(snap) {
  const list = (snap && snap.network && snap.network.interfaces) || [];
  return list
    .filter((it) => (it.totalRx || 0) + (it.totalTx || 0) > 0)
    .sort((a, b) => a.name.localeCompare(b.name));
}

function batteryStatus(b) {
  if (b.charging) return 'Charging';
  if (b.acOnline) return b.percent >= 100 ? 'Fully charged' : 'Plugged in';
  return 'On battery';
}

function timeLeft(secs) {
  const t = Math.max(0, Math.round(secs / 60));
  const h = Math.floor(t / 60);
  const m = t % 60;
  return h > 0 ? `${h}h ${String(m).padStart(2, '0')}m` : `${m} min`;
}

// ---------------------------------------------------------------- markup

function icon(id) {
  return `<svg class="icon" aria-hidden="true"><use href="#${id}"/></svg>`;
}

function tileSpecs(snap) {
  const specs = [
    { key: 'cpu', kind: 'cpu', label: RES.cpu.label },
    { key: 'memory', kind: 'memory', label: RES.memory.label },
  ];
  const disks = (snap && Array.isArray(snap.disks)) ? snap.disks : [];
  if (disks.length) {
    for (const d of disks) {
      const p = diskParts(d.name);
      specs.push({ key: `disk:${d.name}`, kind: 'disk', name: d.name, label: p.label, tag: p.letters.join(' ') });
    }
  } else {
    specs.push({ key: 'disk', kind: 'disk', label: RES.disk.label });
  }
  const ifaces = activeIfaces(snap);
  if (ifaces.length !== 1) {
    specs.push({ key: 'network', kind: 'network', label: RES.network.label, tag: ifaces.length > 1 ? 'All' : '' });
  }
  for (const it of ifaces) specs.push({ key: `net:${it.name}`, kind: 'network', name: it.name, label: it.name });
  if (gpuSeen) specs.push({ key: 'gpu', kind: 'gpu', label: RES.gpu.label });
  if (snap && snap.battery) specs.push({ key: 'battery', kind: 'battery', label: RES.battery.label });
  return specs;
}

function tileHtml(spec, i) {
  const r = RES[spec.kind];
  const full = spec.tag ? `${spec.label} ${spec.tag}` : spec.label;
  return `
    <button type="button" class="tile perf-tile perf-tile--${spec.kind}" role="tab" id="perf-tab-${i}"
      aria-controls="perf-pane-${spec.kind}" aria-selected="false" tabindex="-1"
      data-key="${fmt.escapeHtml(spec.key)}" title="${fmt.escapeHtml(full)}">
      <span class="perf-tile__icon">${icon(r.icon)}</span>
      <span class="perf-tile__text">
        <span class="perf-tile__name">${fmt.escapeHtml(spec.label)}${spec.tag ? `<span class="perf-tile__tag">${fmt.escapeHtml(spec.tag)}</span>` : ''}</span>
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

// opts.span: a fixed span text (the chart does not follow the update speed).
function chartHtml(ref, label, topLabel, opts = {}) {
  const legend = opts.legend || '';
  const size = opts.small ? ' perf-chart--small' : '';
  const span = opts.span || '<span class="js-span">60 seconds</span>';
  return `
    <div class="perf-chart-block">
      <div class="perf-chart-head">
        <p class="micro">${label} / ${span}</p>
        ${legend}
      </div>
      <div class="perf-chart${size}">
        <span class="perf-chart__top" data-ref="${ref}Top">${topLabel}</span>
        <canvas class="perf-chart__canvas" data-ref="${ref}" role="img" aria-label="${label}, recent history"></canvas>
        ${opts.note ? `<p class="perf-chart__note" data-ref="${ref}Note" hidden>${opts.note}</p>` : ''}
      </div>
      <div class="perf-axis" aria-hidden="true"><span>${span} ago</span><span>Now</span></div>
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
        <p class="perf-note">Per-app download and upload rates are in the Network view when Orbit
        runs as administrator. Totals for each network interface are shown here.</p>
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

function factsBlockHtml(key) {
  return `
    <div class="perf-block">
      <div class="perf-block__head"><p class="micro">System facts / ${RES[key].label}</p></div>
      ${factsHtml(key)}
    </div>`;
}

function lowerHtml(key) {
  return `
    <div class="perf-lower">
      ${factsBlockHtml(key)}
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
          <div class="perf-block__head"><p class="micro" data-ref="volHead">Volumes / Capacity</p></div>
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
        <div class="perf-gpu-main">
          ${chartHtml('chart', 'Utilisation', '100%', { note: 'No GPU counters in the latest sample.' })}
          <div class="perf-engines">
            <div class="perf-chart-head">
              <p class="micro">Engines / <span data-ref="engineCount">0</span></p>
            </div>
            <ul class="perf-engine-list" data-ref="engines" aria-label="GPU engines"></ul>
            <p class="perf-note" data-ref="enginesEmpty" hidden>No per-engine counters reported.</p>
          </div>
        </div>
        ${lowerHtml(key)}`;
      break;
    case 'battery':
      body = `
        ${paneHeadHtml(key, idx, 'Battery')}
        ${chartHtml('chart', 'Charge', '100%', {
          span: '2 hours',
          note: 'The charge history starts after the first minute.',
          legend: '<span class="perf-block__hint">One point a minute while Orbit runs</span>',
        })}
        ${factsBlockHtml(key)}`;
      break;
    default:
      break;
  }
  return `<section class="perf-pane perf-pane--${key}" id="perf-pane-${key}" role="tabpanel"
    data-res="${key}" hidden>${body}</section>`;
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
      <div class="perf-tiles" role="tablist" aria-orientation="vertical" aria-label="Resources" data-ref="tiles"></div>
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
  tilesEl = root.querySelector('.perf-tiles');

  for (const key of ORDER) {
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
  charts.battery = new LineChart(panes.battery.refs.chart, {
    series: [{ color: RES.battery.color, fill: true }], max: 100, grid: true, points: BATTERY_POINTS,
  });

  bindEvents();

  wanted = loadSelected();
  if (store.snapshot) {
    if (store.snapshot.gpu && store.snapshot.gpu.usage != null) gpuSeen = true;
    snapAt = Date.now();
    track(store.snapshot);
  }
  buildTiles(store.snapshot);

  syncSettings(store.settings || {});

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
    ro.observe(tilesEl);
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
  tilesEl.addEventListener('click', (e) => {
    const t = e.target.closest('.perf-tile');
    if (t) select(t.dataset.key, false);
  });

  tilesEl.addEventListener('keydown', (e) => {
    if (!['ArrowDown', 'ArrowUp', 'ArrowLeft', 'ArrowRight', 'Home', 'End'].includes(e.key)) return;
    const list = tileList.map((t) => t.el);
    const cur = list.indexOf(e.target.closest('.perf-tile'));
    if (cur < 0) return;
    let next = cur;
    if (e.key === 'ArrowDown' || e.key === 'ArrowRight') next = (cur + 1) % list.length;
    else if (e.key === 'ArrowUp' || e.key === 'ArrowLeft') next = (cur - 1 + list.length) % list.length;
    else if (e.key === 'Home') next = 0;
    else if (e.key === 'End') next = list.length - 1;
    e.preventDefault();
    select(list[next].dataset.key, true);
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

// The tile shown for a wanted key: itself, else the first tile of the same kind, else CPU.
function resolve(key) {
  if (tileMap.has(key)) return key;
  const kind = kindOf(key);
  for (const t of tileList) if (t.kind === kind) return t.key;
  return 'cpu';
}

function applySelection(focusKey) {
  selected = resolve(wanted);
  const kind = kindOf(selected);
  for (const t of tileList) {
    const on = t.key === selected;
    t.el.setAttribute('aria-selected', on ? 'true' : 'false');
    t.el.tabIndex = on ? 0 : -1;
    t.el.classList.toggle('is-active', on);
  }
  for (const key of ORDER) setHidden(panes[key].el, key !== kind);
  const tile = tileMap.get(selected);
  if (tile) panes[kind].el.setAttribute('aria-labelledby', tile.el.id);
  if (focusKey) {
    const f = tileMap.get(focusKey) || tile;
    if (f) f.el.focus();
  }
}

// Rebuild the tile list when the set of tiles changed. Keeps keyboard focus on the same tile.
function buildTiles(snap) {
  const specs = tileSpecs(snap);
  const key = specs.map((s) => s.key).join('\n');
  if (key === layoutKey) return false;
  layoutKey = key;
  const active = document.activeElement;
  const focusKey = active && tilesEl.contains(active) && active.dataset ? active.dataset.key : null;
  tilesEl.innerHTML = specs.map(tileHtml).join('');
  tileList = Array.from(tilesEl.children, (el, i) => ({ ...specs[i], el, ...collectRefs(el) }));
  tileMap = new Map(tileList.map((t) => [t.key, t]));
  tilesEl.classList.toggle('perf-tiles--many', tileList.length > MANY_TILES);
  applySelection(focusKey);
  return true;
}

function select(key, focus) {
  if (!key) return;
  wanted = key;
  saveSelected(key);
  applySelection(focus ? resolve(key) : null);
  if (visible) {
    if (rafId) { cancelAnimationFrame(rafId); rafId = 0; }
    render();
  }
}

function onSnapshotEvent(snap) {
  snap = snap || store.snapshot;
  if (!snap) return;
  snapAt = Date.now();
  if (!gpuSeen && snap.gpu && snap.gpu.usage != null) gpuSeen = true;
  track(snap);
  schedule();
}

// ---------------------------------------------------------------- history (this view's own)

function track(snap) {
  trackDisks(snap);
  trackInterfaces(snap);
  trackBattery(snap);
}

function trackDisks(snap) {
  const list = Array.isArray(snap.disks) ? snap.disks : [];
  for (let i = 0; i < list.length; i++) {
    const d = list[i];
    let h = diskHist.get(d.name);
    if (!h) { h = { active: [], read: [], write: [], sum: [] }; diskHist.set(d.name, h); }
    ring(h.active, d.activePct);
    ring(h.read, d.readBps);
    ring(h.write, d.writeBps);
    ring(h.sum, num(d.readBps) + num(d.writeBps));
  }
  // An empty list (counters failed for a tick) keeps the history.
  if (list.length && diskHist.size > list.length) {
    for (const name of diskHist.keys()) {
      if (!list.some((d) => d.name === name)) diskHist.delete(name);
    }
  }
}

function trackInterfaces(snap) {
  const list = (snap.network && snap.network.interfaces) || [];
  for (let i = 0; i < list.length; i++) {
    const it = list[i];
    let h = ifaceHist.get(it.name);
    if (!h) { h = { rx: [], tx: [], sum: [] }; ifaceHist.set(it.name, h); }
    ring(h.rx, it.rxBps);
    ring(h.tx, it.txBps);
    ring(h.sum, num(it.rxBps) + num(it.txBps));
  }
  if (ifaceHist.size > list.length) {
    for (const name of ifaceHist.keys()) {
      if (!list.some((it) => it.name === name)) ifaceHist.delete(name);
    }
  }
}

// One point a minute; the newest point follows the live value until the next minute starts.
function trackBattery(snap) {
  const b = snap.battery;
  if (!b) return;
  const now = Date.now();
  if (!batteryHist.length || now - batteryAt >= BATTERY_STEP_MS) {
    ring(batteryHist, b.percent, BATTERY_POINTS);
    batteryAt = now;
  } else {
    batteryHist[batteryHist.length - 1] = num(b.percent);
  }
}

function tickUptime() {
  if (!visible || kindOf(selected) !== 'cpu') return;
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
  buildTiles(snap);
  applyStatic();
  fillBuffers(snap);
  renderTiles(snap);
  switch (kindOf(selected)) {
    case 'cpu': renderCpu(snap); break;
    case 'memory': renderMemory(snap); break;
    case 'disk': renderDisk(snap); break;
    case 'network': renderNetwork(snap); break;
    case 'gpu': renderGpu(snap); break;
    case 'battery': renderBattery(snap); break;
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

// The disk a disk key stands for, with its history: a physical disk, or the system total.
function diskSource(snap, key) {
  if (key.startsWith('disk:')) {
    const name = key.slice(5);
    const d = (snap.disks || []).find((x) => x.name === name);
    const h = diskHist.get(name);
    if (d && h) return { d, h, name };
  }
  return { d: snap.disk || {}, h: { active: buf.diskActive, read: buf.diskRead, write: buf.diskWrite, sum: buf.diskSum }, name: null };
}

// The interface a network key stands for, with its history: one interface, or the total.
function netSource(snap, key) {
  const n = snap.network || {};
  if (key.startsWith('net:')) {
    const name = key.slice(4);
    const it = (n.interfaces || []).find((x) => x.name === name);
    const h = ifaceHist.get(name);
    if (it && h) return { rxBps: num(it.rxBps), txBps: num(it.txBps), it, h };
  }
  return { rxBps: num(n.rxBps), txBps: num(n.txBps), it: null, h: { rx: buf.rx, tx: buf.tx, sum: buf.net } };
}

function renderTiles(snap) {
  for (let i = 0; i < tileList.length; i++) {
    const t = tileList[i];
    switch (t.kind) {
      case 'cpu': renderCpuTile(t, snap); break;
      case 'memory': renderMemoryTile(t, snap); break;
      case 'disk': renderDiskTile(t, snap); break;
      case 'network': renderNetTile(t, snap); break;
      case 'gpu': renderGpuTile(t, snap); break;
      case 'battery': renderBatteryTile(t, snap); break;
      default: break;
    }
  }
}

function renderCpuTile(t, snap) {
  const cpu = snap.cpu;
  setText(t.v, fmt.pct(cpu.usage, 0));
  setText(t.s, cpu.freqMhz > 0 ? ghz(cpu.freqMhz) : `${(cpu.perCore || []).length} logical processors`);
  sparkline(t.spark, buf.cpu, { color: RES.cpu.color, max: 100 });
}

function renderMemoryTile(t, snap) {
  const m = snap.memory;
  setText(t.v, fmt.pct(memPct(m), 0));
  setText(t.s, `${fmt.bytes(m.used)} / ${fmt.bytes(m.total)}`);
  sparkline(t.spark, buf.mem, { color: RES.memory.color, max: 100 });
}

function renderDiskTile(t, snap) {
  const { d, h } = diskSource(snap, t.key);
  setText(t.s, `R ${diskRate(d.readBps)} · W ${diskRate(d.writeBps)}`);
  if (d.activePct != null) {
    setText(t.v, fmt.pct(d.activePct, 0));
    sparkline(t.spark, h.active, { color: RES.disk.color, max: 100 });
  } else {
    setText(t.v, diskSum(d));
    sparkline(t.spark, h.sum, { color: RES.disk.color, max: maxOf(h.sum, 1024) });
  }
}

function renderNetTile(t, snap) {
  const s = netSource(snap, t.key);
  setText(t.v, fmt.rate(s.rxBps + s.txBps));
  setText(t.s, `R ${fmt.rate(s.rxBps)} · S ${fmt.rate(s.txBps)}`);
  sparkline(t.spark, s.h.sum, { color: RES.network.color, max: maxOf(s.h.sum, 1024) });
}

function renderGpuTile(t, snap) {
  const gpu = snap.gpu;
  if (gpu && gpu.usage != null) {
    setText(t.v, fmt.pct(gpu.usage, 0));
    setText(t.s, gpu.dedicatedUsed != null ? `${fmt.bytes(gpu.dedicatedUsed)} dedicated` : 'Dedicated memory not reported');
  } else {
    setText(t.v, 'No GPU counters');
    setText(t.s, 'Counters not reported');
  }
  sparkline(t.spark, buf.gpu, { color: RES.gpu.color, max: 100 });
}

function renderBatteryTile(t, snap) {
  const b = snap.battery;
  if (!b) return;
  setText(t.v, fmt.pct(b.percent, 0));
  setText(t.s, !b.acOnline && !b.charging && b.secondsLeft != null ? `${timeLeft(b.secondsLeft)} left` : batteryStatus(b));
  sparkline(t.spark, batteryHist, { color: RES.battery.color, max: 100, points: BATTERY_POINTS });
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
  const { d, h, name } = diskSource(snap, selected);
  const hasActive = d.activePct != null;
  const si = currentStatic();
  const vols = (si && si.disks) || [];
  const parts = name != null ? diskParts(name) : null;

  if (parts) {
    setText(P.refs.title, parts.letters.length ? `${parts.label} (${parts.letters.join(' ')})` : parts.label);
  } else {
    setText(P.refs.title, vols.length === 1 ? '1 volume' : `${vols.length} volumes`);
  }

  charts.diskActive.draw([h.active]);
  setHidden(P.refs.chartNote, hasActive);
  charts.diskRate.draw([h.read, h.write]);
  setText(P.refs.rateChartTop, `Peak ${fmt.rate(Math.max(maxOf(h.read), maxOf(h.write)))}`);
  setText(P.refs.lRead, diskRate(d.readBps));
  setText(P.refs.lWrite, diskRate(d.writeBps));

  setText(P.refs.headline, hasActive ? fmt.pct(d.activePct, 0) : diskSum(d));
  setText(P.refs.headSub, hasActive
    ? `active · R ${diskRate(d.readBps)} · W ${diskRate(d.writeBps)}`
    : `R ${diskRate(d.readBps)} · W ${diskRate(d.writeBps)}`);

  setFact(P, 'read', diskRate(d.readBps));
  setFact(P, 'write', diskRate(d.writeBps));
  setFact(P, 'active', hasActive ? fmt.pct(d.activePct, 1) : 'Not available');
  if (parts) setFact(P, 'volumes', parts.letters.length ? parts.letters.join(', ') : 'No drive letters');
  else setFact(P, 'volumes', count(si && si.disks ? si.disks.length : null));

  renderVolumes(si, parts ? parts.letters : null);
  renderTop(P, 'disk', snap);
}

// Capacity bars: the volumes on the selected disk (by drive letter), or every volume.
function renderVolumes(si, letters) {
  const lk = letters ? letters.join(' ') : null;
  if (volumesFor && volumesFor[0] === si && volumesFor[1] === lk) return;
  volumesFor = [si, lk];
  const P = panes.disk;
  const all = (si && si.disks) || [];
  const disks = letters
    ? all.filter((d) => letters.includes(String(d.mount || '').slice(0, 2).toUpperCase()))
    : all;
  setText(P.refs.volHead, letters ? 'Volumes / On this disk' : 'Volumes / Capacity');
  if (!disks.length) {
    P.refs.disks.innerHTML = `<li class="perf-note">${!si ? 'Loading volumes.'
      : letters ? 'No volumes with a drive letter on this disk.' : 'No volumes reported.'}</li>`;
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
  const s = netSource(snap, selected);

  setText(P.refs.title, s.it ? s.it.name : 'Network');
  charts.network.draw([s.h.rx, s.h.tx]);
  setText(P.refs.chartTop, `Peak ${fmt.rate(Math.max(maxOf(s.h.rx), maxOf(s.h.tx)))}`);
  setText(P.refs.lRx, fmt.rate(s.rxBps));
  setText(P.refs.lTx, fmt.rate(s.txBps));

  setText(P.refs.headline, fmt.rate(s.rxBps));
  setText(P.refs.headSub, `received · sent ${fmt.rate(s.txBps)}`);

  let totRx = 0;
  let totTx = 0;
  let active = 0;
  for (let i = 0; i < list.length; i++) {
    totRx += list[i].totalRx || 0;
    totTx += list[i].totalTx || 0;
    if ((list[i].totalRx || 0) + (list[i].totalTx || 0) > 0) active++;
  }
  setFact(P, 'rx', fmt.rate(s.rxBps));
  setFact(P, 'tx', fmt.rate(s.txBps));
  setFact(P, 'totalRx', fmt.bytes(s.it ? s.it.totalRx || 0 : totRx));
  setFact(P, 'totalTx', fmt.bytes(s.it ? s.it.totalTx || 0 : totTx));
  setFact(P, 'ifaces', `${active} of ${list.length}`);

  renderInterfaces(list, s.it ? s.it.name : null);
}

function renderInterfaces(list, selName) {
  const P = panes.network;
  let key = '';
  for (let i = 0; i < list.length; i++) key += `${list[i].name}\n`;
  if (key !== ifaceKey) {
    ifaceKey = key;
    ifaceSel = undefined;
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
        row,
        rx: row.querySelector('[data-k="rx"]'),
        tx: row.querySelector('[data-k="tx"]'),
        tot: row.querySelector('[data-k="tot"]'),
        spark: row.querySelector('canvas'),
      });
    });
    setText(P.refs.ifaceCount, String(list.length));
    setHidden(P.refs.ifaceEmpty, list.length > 0);
  }
  if (selName !== ifaceSel) {
    ifaceSel = selName;
    for (const [name, r] of ifaceRows) r.row.classList.toggle('is-selected', name === selName);
  }
  for (let i = 0; i < list.length; i++) {
    const it = list[i];
    const r = ifaceRows.get(it.name);
    if (!r) continue;
    setText(r.rx, fmt.rate(it.rxBps));
    setText(r.tx, fmt.rate(it.txBps));
    setText(r.tot, `${fmt.bytes(it.totalRx)} / ${fmt.bytes(it.totalTx)}`);
    const h = ifaceHist.get(it.name);
    if (h) sparkline(r.spark, h.sum, { color: RES.network.color, max: maxOf(h.sum, 1024) });
  }
}

// ---- GPU

function renderGpu(snap) {
  const P = panes.gpu;
  const g = snap.gpu || {};
  const has = g.usage != null;
  const engines = Array.isArray(snap.gpuEngines) ? snap.gpuEngines : [];

  charts.gpu.draw([buf.gpu]);
  setHidden(P.refs.chartNote, has);
  setText(P.refs.headline, has ? fmt.pct(g.usage, 0) : 'No GPU counters');
  setText(P.refs.headSub, g.dedicatedUsed != null ? `${fmt.bytes(g.dedicatedUsed)} dedicated memory` : 'Dedicated memory not reported');

  renderEngines(engines);

  let busy = null;
  for (let i = 0; i < engines.length; i++) if (!busy || engines[i].usage > busy.usage) busy = engines[i];
  setFact(P, 'usage', has ? fmt.pct(g.usage, 1) : 'No GPU counters');
  setFact(P, 'vram', g.dedicatedUsed != null ? fmt.bytes(g.dedicatedUsed) : 'Not available');
  setFact(P, 'engine', busy ? `${busy.name} · ${fmt.pct(busy.usage, 1)}` : 'Not available');
  setFact(P, 'peak', fmt.pct(maxOf(buf.gpu), 1));
  setFact(P, 'avg', fmt.pct(avgOf(buf.gpu), 1));

  renderTop(P, 'gpu', snap);
}

// Per-engine bars beside the main chart (the busiest engine of each type, 0-100).
function renderEngines(engines) {
  const P = panes.gpu;
  let key = '';
  for (let i = 0; i < engines.length; i++) key += `${engines[i].name}\n`;
  if (key !== engineKey) {
    engineKey = key;
    P.refs.engines.innerHTML = engines.map((e) => `
      <li class="perf-engine">
        <span class="perf-engine__name" title="${fmt.escapeHtml(e.name)}">${fmt.escapeHtml(e.name)}</span>
        <span class="perf-engine__bar" aria-hidden="true"><span class="perf-engine__fill"></span></span>
        <span class="perf-engine__val"></span>
      </li>`).join('');
    engineRows = Array.from(P.refs.engines.children, (li) => ({
      val: li.querySelector('.perf-engine__val'),
      fill: li.querySelector('.perf-engine__fill'),
      last: -1,
    }));
    setText(P.refs.engineCount, String(engines.length));
    setHidden(P.refs.engines, !engines.length);
    setHidden(P.refs.enginesEmpty, engines.length > 0);
  }
  for (let i = 0; i < engines.length; i++) {
    const r = engineRows[i];
    const v = Math.min(100, Math.max(0, num(engines[i].usage)));
    setText(r.val, fmt.pct(v, 0));
    const frac = Math.round(v * 10) / 1000;
    if (frac !== r.last) { r.fill.style.transform = `scaleX(${frac})`; r.last = frac; }
  }
}

// ---- Battery

function renderBattery(snap) {
  const P = panes.battery;
  const b = snap.battery;
  charts.battery.draw([batteryHist]);
  setHidden(P.refs.chartNote, batteryHist.length > 1);
  if (!b) return; // the tile is gone; the next render moves the selection
  const status = batteryStatus(b);
  const left = b.secondsLeft != null ? `${timeLeft(b.secondsLeft)} left` : null;

  setText(P.refs.headline, fmt.pct(b.percent, 0));
  setText(P.refs.headSub, left && !b.acOnline ? `${status.toLowerCase()} · ${left}` : status.toLowerCase());

  setFact(P, 'charge', fmt.pct(b.percent, 0));
  setFact(P, 'status', status);
  setFact(P, 'left', b.secondsLeft != null ? timeLeft(b.secondsLeft)
    : b.acOnline ? 'Plugged in' : 'Not estimated yet');
  setFact(P, 'source', b.acOnline ? 'Power adapter' : 'Battery');
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

  if (P.refs.topHint) setText(P.refs.topHint, q ? `Matching "${q}"` : key === 'disk' ? 'By I/O rate, all disks' : `By ${RES[key].metric} use`);
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

// ---- Static info (CPU brand; volumes are drawn by renderDisk)

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
}
