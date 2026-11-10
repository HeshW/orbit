// 10 / My Rig: one read-only spec sheet for this PC, with a Copy specs button.
//
// get_rig_specs (rig.rs) reads everything once per run (WMI, registry, DXGI,
// QueryDisplayConfig; no admin) and caches it, so the view asks once and keeps the answer.
// Cards: Processor, Memory, Graphics, Motherboard, Storage, Displays, Windows. The head shows
// four readouts (cores / threads, memory, video memory, storage). Copy specs puts a plain-text
// summary on the clipboard, one line per part, for forums and support chats.

import { call } from '../api.js';
import { toast, navigate } from '../app.js';
import { escapeHtml } from '../format.js';
import { registerCommands } from '../palette.js';
import { copyText } from './processes.js';

const GIB = 1024 ** 3;
const DASH = '–';

let root = null;
const els = {};
let visible = false;
let specs = null;
let loading = false;
let error = null;
let unregister = null;

const icon = (id) => `<svg class="icon" aria-hidden="true"><use href="#${id}"/></svg>`;
const text = (s) => escapeHtml(s == null ? '' : String(s));

function errText(err) {
  if (typeof err === 'string' && err) return err;
  if (err && typeof err.message === 'string' && err.message) return err.message;
  return 'Something went wrong. Try again.';
}

// ------------------------------------------------------------------ formatting

/** "Intel(R) Core(TM) i7-8700 CPU @ 3.20GHz" -> "Intel Core i7-8700". */
export function tidyName(name) {
  return String(name || '')
    .replace(/\((R|TM|C)\)/gi, '')
    .replace(/\s+CPU\s*@.*$/i, '')
    .replace(/\s+@\s*[\d.]+\s*GHz$/i, '')
    .replace(/\s+\d+-Core Processor$/i, '')
    .replace(/\s+/g, ' ')
    .trim();
}

/** Memory and video memory: binary, whole GB from 1 GB up ("16 GB", "512 MB"). */
export function memSize(bytes) {
  const n = Number(bytes) || 0;
  if (n >= GIB) return `${Math.round(n / GIB)} GB`;
  return `${Math.max(1, Math.round(n / 1024 ** 2))} MB`;
}

/** Drives: decimal, as printed on the box ("240 GB", "1 TB", "1.5 TB"). */
export function driveSize(bytes) {
  const n = Number(bytes) || 0;
  if (n >= 1e12) return `${Number((n / 1e12).toFixed(1))} TB`;
  return `${Math.round(n / 1e9)} GB`;
}

function ghz(mhz) {
  return `${(mhz / 1000).toFixed(2)} GHz`;
}

function hz(v) {
  const n = Number(v) || 0;
  return `${Number.isInteger(n) ? n : n.toFixed(2)} Hz`;
}

/** [16 GB, 16 GB] -> "2 x 16 GB"; mixed sizes -> "16 GB + 8 GB". */
export function moduleText(modules) {
  const list = (modules || []).map(memSize);
  if (!list.length) return '';
  return list.every((m) => m === list[0]) ? `${list.length} x ${list[0]}` : list.join(' + ');
}

function slotsText(m) {
  if (m.slotsTotal) return `${m.slotsUsed} of ${m.slotsTotal}`;
  return m.slotsUsed ? `${m.slotsUsed} used` : '';
}

function driveKind(d) {
  const media = d.media && d.media !== 'Unknown' ? d.media : '';
  return [d.bus, media].filter(Boolean).join(' ');
}

function boardName(b) {
  return [b.maker, b.model].filter(Boolean).join(' ');
}

function pcName(b) {
  return [b.systemMaker, b.systemModel].filter(Boolean).join(' ');
}

function windowsName(w) {
  return [w.edition, w.version].filter(Boolean).join(' ');
}

/** The Copy specs text: one plain line per part, no decoration. */
export function specsText(s) {
  const lines = [];
  const pc = pcName(s.board);
  if (pc) lines.push(`PC: ${pc}`);
  const c = s.cpu;
  if (c.name) {
    const extra = [
      c.cores ? `${c.cores} cores` : '',
      c.threads ? `${c.threads} threads` : '',
      c.baseMhz ? `${ghz(c.baseMhz)} base` : '',
    ].filter(Boolean).join(', ');
    lines.push(`CPU: ${tidyName(c.name)}${extra ? ` (${extra})` : ''}`);
  }
  const m = s.memory;
  if (m.total) {
    const main = [memSize(m.total), m.kind, m.speed ? `${m.speed} MT/s` : ''].filter(Boolean).join(' ');
    const extra = [moduleText(m.modules), m.slotsTotal ? `${m.slotsUsed} of ${m.slotsTotal} slots` : ''].filter(Boolean).join(', ');
    lines.push(`Memory: ${main}${extra ? ` (${extra})` : ''}`);
  }
  for (const g of s.gpus || []) {
    const extra = [g.dedicatedMemory > 0 ? memSize(g.dedicatedMemory) : '', g.driverVersion ? `driver ${g.driverVersion}` : '']
      .filter(Boolean).join(', ');
    lines.push(`GPU: ${tidyName(g.name)}${extra ? ` (${extra})` : ''}`);
  }
  const board = boardName(s.board);
  if (board) {
    const bios = [s.board.biosVersion ? `BIOS ${s.board.biosVersion}` : '', s.board.biosDate || ''].filter(Boolean).join(', ');
    lines.push(`Motherboard: ${board}${bios ? ` (${bios})` : ''}`);
  }
  for (const d of s.drives || []) {
    const kind = driveKind(d);
    lines.push(`Drive: ${d.name}, ${driveSize(d.size)}${kind ? ` ${kind}` : ''}`);
  }
  for (const d of s.displays || []) {
    lines.push(`Display: ${d.width} x ${d.height} at ${hz(d.refreshHz)}${d.name ? ` (${d.name})` : ''}`);
  }
  const win = windowsName(s.windows);
  if (win) lines.push(`Windows: ${win}${s.windows.build ? ` (build ${s.windows.build})` : ''}`);
  return lines.join('\n');
}

// ------------------------------------------------------------------ data

async function load() {
  if (loading || specs) return;
  loading = true;
  error = null;
  render();
  try {
    specs = await call('get_rig_specs');
  } catch (err) {
    error = errText(err);
  }
  loading = false;
  render();
}

async function copySpecs() {
  if (!specs) {
    toast('The specs are still loading.', 'info');
    return;
  }
  const ok = await copyText(specsText(specs));
  toast(ok ? 'Copied your specs.' : 'Could not copy to the clipboard.', ok ? 'success' : 'error');
}

// ------------------------------------------------------------------ render

function fact(label, value) {
  return `<div class="rig-fact"><dt class="micro">${text(label)}</dt><dd>${value ? text(value) : DASH}</dd></div>`;
}

function card(id, label, ic, body) {
  return `
    <section class="panel rig-card rig-card--${id}" aria-labelledby="rig-h-${id}">
      <div class="rig-card__head">
        <span class="rig-card__icon">${icon(ic)}</span>
        <h2 class="micro micro--bright rig-card__label" id="rig-h-${id}">${text(label)}</h2>
      </div>
      ${body}
    </section>`;
}

function title(value, sub = '') {
  return `<p class="rig-card__title" title="${text(value)}">${value ? text(value) : 'Not reported'}</p>
    ${sub ? `<p class="rig-card__sub">${text(sub)}</p>` : ''}`;
}

function listHtml(items, empty) {
  if (!items.length) return `<p class="rig-note">${text(empty)}</p>`;
  return `<ul class="rig-list">${items.map((it) => `
    <li class="rig-row">
      <span class="rig-row__name" title="${text(it.name)}">${text(it.name)}</span>
      <span class="rig-row__vals">${it.vals.filter(Boolean).map((v) => `<span>${text(v)}</span>`).join('')}</span>
      ${it.chips.filter(Boolean).map((c) => `<span class="chip rig-chip">${text(c)}</span>`).join('')}
    </li>`).join('')}</ul>`;
}

function cardsHtml(s) {
  const c = s.cpu;
  const m = s.memory;
  const b = s.board;
  const w = s.windows;
  const pc = pcName(b);
  return [
    card('cpu', 'Processor', 'i-cpu', `
      ${title(c.name ? tidyName(c.name) : '', c.sockets > 1 ? `${c.sockets} processors` : '')}
      <dl class="rig-facts">
        ${fact('Cores', c.cores)}
        ${fact('Threads', c.threads)}
        ${fact('Base clock', c.baseMhz ? ghz(c.baseMhz) : '')}
      </dl>`),
    card('memory', 'Memory', 'i-memory', `
      ${title(m.total ? memSize(m.total) : '', moduleText(m.modules))}
      <dl class="rig-facts">
        ${fact('Type', m.kind)}
        ${fact('Speed', m.speed ? `${m.speed} MT/s` : '')}
        ${fact('Slots used', slotsText(m))}
      </dl>`),
    card('gpu', 'Graphics', 'i-gpu', listHtml((s.gpus || []).map((g) => ({
      name: tidyName(g.name),
      vals: [g.dedicatedMemory > 0 ? `${memSize(g.dedicatedMemory)} VRAM` : 'Shared memory', g.driverVersion ? `Driver ${g.driverVersion}` : ''],
      chips: [],
    })), 'Windows did not name a graphics adapter.')),
    card('board', 'Motherboard', 'i-grid', `
      ${title(boardName(b), pc ? `In ${pc}` : '')}
      <dl class="rig-facts">
        ${fact('BIOS', b.biosVersion)}
        ${fact('BIOS date', b.biosDate)}
        ${fact('BIOS maker', b.biosVendor)}
      </dl>`),
    card('drives', 'Storage', 'i-disk', listHtml((s.drives || []).map((d) => ({
      name: d.name,
      vals: [driveSize(d.size)],
      chips: [d.bus, d.media && d.media !== 'Unknown' ? d.media : ''],
    })), 'Windows did not list any drives.')),
    card('displays', 'Displays', 'i-mini', listHtml((s.displays || []).map((d) => ({
      name: d.name || (d.internal ? 'Built-in display' : 'Display'),
      vals: [`${d.width} x ${d.height}`, hz(d.refreshHz)],
      chips: [d.primary && s.displays.length > 1 ? 'Main' : ''],
    })), 'No active display reported.')),
    card('windows', 'Windows', 'i-shield', `
      ${title(w.edition, '')}
      <dl class="rig-facts">
        ${fact('Version', w.version)}
        ${fact('Build', w.build)}
      </dl>`),
  ].join('');
}

function renderStats() {
  const s = specs;
  const set = (el, v) => { el.textContent = v || DASH; };
  if (!s) {
    for (const el of [els.statCores, els.statMem, els.statVram, els.statDisk]) set(el, '');
    return;
  }
  set(els.statCores, s.cpu.cores && s.cpu.threads ? `${s.cpu.cores} / ${s.cpu.threads}` : '');
  set(els.statMem, s.memory.total ? memSize(s.memory.total) : '');
  const vram = (s.gpus || []).reduce((max, g) => Math.max(max, g.dedicatedMemory || 0), 0);
  set(els.statVram, vram >= GIB ? memSize(vram) : '');
  const disk = (s.drives || []).reduce((sum, d) => sum + (d.size || 0), 0);
  set(els.statDisk, disk ? driveSize(disk) : '');
}

function render() {
  if (!root) return;
  renderStats();
  els.copy.disabled = !specs;
  if (specs) {
    els.empty.hidden = true;
    els.grid.hidden = false;
    els.grid.innerHTML = cardsHtml(specs);
    return;
  }
  els.grid.hidden = true;
  els.empty.hidden = false;
  els.empty.innerHTML = error
    ? `${icon('i-x')}<p class="empty-title">Could not read this PC's specs.</p><p>${text(error)}</p>
       <button type="button" class="btn btn--ghost btn--sm" data-retry>Try again</button>`
    : `${icon('i-cpu')}<p class="empty-title">Reading your hardware.</p><p>This takes a second, once per run.</p>`;
}

// ------------------------------------------------------------------ view API

function paletteItems() {
  return [{
    label: 'Copy specs',
    search: 'copy specs my rig hardware cpu gpu ram system info share',
    icon: 'i-copy',
    hint: 'My Rig',
    run: async () => {
      if (!specs) {
        navigate('rig');
        await load();
      }
      copySpecs();
    },
  }];
}

export function mount(rootEl) {
  root = rootEl;
  root.innerHTML = `
  <div class="rig">
    <header class="view-head">
      <div class="lv-head-text">
        <p class="micro">10 / My Rig</p>
        <h1 class="view-title">Your PC. <em>On one page.</em></h1>
      </div>
      <div class="lv-stats">
        <div class="stat lv-stat"><span class="micro stat-label">Cores / threads</span><span class="stat-value lv-stat-value" data-stat="cores">${DASH}</span></div>
        <div class="stat lv-stat"><span class="micro stat-label">Memory</span><span class="stat-value lv-stat-value" data-stat="mem">${DASH}</span></div>
        <div class="stat lv-stat"><span class="micro stat-label">VRAM</span><span class="stat-value lv-stat-value" data-stat="vram">${DASH}</span></div>
        <div class="stat lv-stat"><span class="micro stat-label">Storage</span><span class="stat-value lv-stat-value" data-stat="disk">${DASH}</span></div>
      </div>
    </header>
    <div class="toolbar rig-toolbar">
      <p class="rig-lede">Read from Windows on this PC. Nothing is sent anywhere.</p>
      <span class="lv-toolbar-spacer"></span>
      <button type="button" class="btn btn--primary btn--sm" data-copy disabled>${icon('i-copy')}<span>Copy specs</span></button>
    </div>
    <div class="rig-grid" hidden></div>
    <div class="empty rig-empty"></div>
  </div>`;
  els.grid = root.querySelector('.rig-grid');
  els.empty = root.querySelector('.rig-empty');
  els.copy = root.querySelector('[data-copy]');
  els.statCores = root.querySelector('[data-stat="cores"]');
  els.statMem = root.querySelector('[data-stat="mem"]');
  els.statVram = root.querySelector('[data-stat="vram"]');
  els.statDisk = root.querySelector('[data-stat="disk"]');
  els.copy.addEventListener('click', copySpecs);
  els.empty.addEventListener('click', (e) => {
    if (e.target instanceof Element && e.target.closest('[data-retry]')) load();
  });
  if (!unregister) unregister = registerCommands(paletteItems);
  render();
}

export function onShow() {
  visible = true;
  load();
}

export function onHide() {
  visible = false;
}

export function isVisible() {
  return visible && !!root;
}
