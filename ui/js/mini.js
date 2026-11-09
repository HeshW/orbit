// Orbit / mini: the always-on-top mini window (ui/mini.html, window label "mini").
// Shell feature, docs/V04-CONTRACT.md. Live CPU, memory, disk, network and GPU with a
// 60-sample sparkline each, from orbit://snapshot (full Snapshot or SnapshotDelta: both
// carry cpu, memory, disk, network and gpu). "Open Orbit" sends orbit://shell-show;
// the close button (and Esc) call toggle_mini. The theme follows the main window through
// localStorage 'orbit.settings'.

import { call, onSnapshot, isTauri } from './api.js';
import { sparkline } from './charts.js';
import { pct, rate } from './format.js';

const LEN = 60;
const THEMES = ['night', 'void', 'signal', 'daylight'];
const T = typeof window !== 'undefined' ? window.__TAURI__ : undefined;

const $ = (id) => document.getElementById(id);
const rings = { cpu: [], mem: [], disk: [], net: [], gpu: [] };
const canvases = {};
for (const c of document.querySelectorAll('canvas[data-m]')) canvases[c.dataset.m] = c;
const values = { cpu: $('sh-mini-cpu'), mem: $('sh-mini-mem'), disk: $('sh-mini-disk'), net: $('sh-mini-net'), gpu: $('sh-mini-gpu') };
const SPARK = {
  cpu: { color: '--chart-cpu', max: 100 },
  mem: { color: '--chart-mem', max: 100 },
  disk: { color: '--chart-disk', max: 100 },
  net: { color: '--chart-net', max: 'auto' },
  gpu: { color: '--chart-gpu', max: 100 },
};
let diskIsRate = false;
let lastTs = 0;

function push(key, v) {
  const r = rings[key];
  r.push(Number.isFinite(v) ? v : 0);
  if (r.length > LEN) r.shift();
}

function setText(el, text) {
  if (el && el.textContent !== text) el.textContent = text;
}

function apply(snap) {
  if (!snap || !snap.cpu || !snap.memory) return;
  if (snap.ts && snap.ts <= lastTs) return;
  lastTs = snap.ts || 0;

  const cpu = snap.cpu.usage;
  const memPct = snap.memory.total > 0 ? (snap.memory.used / snap.memory.total) * 100 : null;
  const disk = snap.disk || {};
  const net = snap.network || {};
  const gpu = snap.gpu ? snap.gpu.usage : null;

  // Disk active time when Windows reports it, else read + write per second.
  const active = disk.activePct;
  const nowRate = !Number.isFinite(active);
  if (nowRate !== diskIsRate) {
    diskIsRate = nowRate;
    rings.disk.length = 0;
    SPARK.disk.max = nowRate ? 'auto' : 100;
  }
  const diskRate = (disk.readBps || 0) + (disk.writeBps || 0);
  const netRate = (net.rxBps || 0) + (net.txBps || 0);

  push('cpu', cpu);
  push('mem', memPct);
  push('disk', nowRate ? diskRate : active);
  push('net', netRate);
  push('gpu', gpu);

  setText(values.cpu, pct(cpu, 0));
  setText(values.mem, pct(memPct, 0));
  setText(values.disk, nowRate ? (disk.readBps == null && disk.writeBps == null ? '–' : rate(diskRate)) : pct(active, 0));
  setText(values.net, rate(netRate));
  setText(values.gpu, gpu == null ? '–' : pct(gpu, 0));
  draw();
}

function draw() {
  if (document.hidden) return;
  for (const key of Object.keys(rings)) {
    sparkline(canvases[key], rings[key], { color: SPARK[key].color, max: SPARK[key].max, points: LEN });
  }
}

function readSettings() {
  try {
    return JSON.parse(localStorage.getItem('orbit.settings') || '{}') || {};
  } catch {
    return {};
  }
}

function applySettings() {
  const s = readSettings();
  const root = document.documentElement;
  const theme = THEMES.includes(s.theme) ? s.theme : 'night';
  if (root.dataset.theme !== theme) {
    root.dataset.theme = theme;
    requestAnimationFrame(draw);
  }
  root.classList.toggle('reduce-motion', !!s.reducedMotion);
  const state = $('sh-mini-state');
  setText(state, s.paused ? 'Paused' : 'Live');
  if (state) state.classList.toggle('is-paused', !!s.paused);
  const fav = document.querySelector('link[rel="icon"]');
  if (fav) fav.setAttribute('href', theme === 'daylight' ? 'assets/orbit-mark-light.svg' : 'assets/orbit-mark-dark.svg');
}

function openOrbit() {
  if (isTauri && T.event) {
    T.event.emit('orbit://shell-show').catch((err) => console.error('[orbit] shell-show failed', err));
  } else {
    location.href = 'index.html';
  }
}

function closeMini() {
  call('toggle_mini').catch((err) => console.error('[orbit] toggle_mini failed', err));
  if (!isTauri) window.close();
}

$('sh-mini-open')?.addEventListener('click', openOrbit);
$('sh-mini-close')?.addEventListener('click', closeMini);
document.addEventListener('keydown', (ev) => {
  if (ev.key === 'Escape') closeMini();
});
window.addEventListener('storage', (ev) => {
  if (!ev.key || ev.key === 'orbit.settings') applySettings();
});
document.addEventListener('visibilitychange', draw);

applySettings();
onSnapshot(apply);
// Fill the readout now instead of waiting a tick (or forever, when updates are paused).
call('get_snapshot').then(apply).catch(() => {});
$('mini-root').dataset.ready = 'true';
