// Optimize (0.5): PC health, disk cleanup, game boost and driver check, one sub-tab each.
// This file is only the frame: the view head, the tab list and one pane per sub-tab.
// Each sub-tab lives in its own module, ui/js/views/optimize/<id>.js, with the API
//   export function mount(paneEl, ctx)   // once, the first time its tab opens
//   export function onShow()             // optional: the tab became visible
//   export function onHide()             // optional: the tab or the view was left
// ctx = { openTab(id), setBadge(text, kind?) } (kind: '' | 'warn' | 'danger' tints the
// count next to the tab label). Styles go in ui/css/views/optimize/<id>.css.
// To add a sub-tab: add it to TABS, create the module and its CSS, and link the CSS in
// index.html. Nothing else here needs to change.
import { navigate, currentViewName } from '../app.js';
import { registerCommands } from '../palette.js';

const TABS = [
  { id: 'health', label: 'Health', title: 'Check up. <em>Fix what matters.</em>', search: 'pc health report disk updates restart' },
  { id: 'cleanup', label: 'Cleanup', title: 'Clear the junk. <em>Keep your files.</em>', search: 'disk cleanup temp files free space junk' },
  { id: 'boost', label: 'Boost', title: 'Focus the PC. <em>Undo it after.</em>', search: 'game boost focus mode power plan' },
  { id: 'drivers', label: 'Drivers', title: 'Know your drivers. <em>Update safely.</em>', search: 'drivers devices driver check' },
];
const TAB_KEY = 'orbit.optimizeTab';

let root = null;
let visible = false;
let tab = readTab();
const panes = {}; // id -> { el, mod, mounted, loading, failed }
const els = {};

function readTab() {
  try {
    const t = localStorage.getItem(TAB_KEY);
    if (TABS.some((x) => x.id === t)) return t;
  } catch {
    /* storage unavailable */
  }
  return 'health';
}

function saveTab() {
  try {
    localStorage.setItem(TAB_KEY, tab);
  } catch {
    /* storage unavailable */
  }
}

function ctxFor(id) {
  return {
    openTab,
    setBadge(text, kind = '') {
      const c = els.tabs?.querySelector(`[data-tab="${id}"] .ar-tab-count`);
      if (!c) return;
      c.textContent = text == null ? '' : String(text);
      c.classList.toggle('ar-tab-count--warn', kind === 'warn' || kind === 'danger');
    },
  };
}

function failPane(p, err) {
  console.error('[orbit] optimize tab failed to load', err);
  p.failed = true;
  p.el.innerHTML = `<div class="empty"><svg class="icon" aria-hidden="true"><use href="#i-x"/></svg>
    <strong>This tab did not load.</strong><p></p></div>`;
  p.el.querySelector('p').textContent = String(err?.message || err);
}

async function ensureMounted(id) {
  const p = panes[id];
  if (p.mounted || p.loading || p.failed) return;
  p.loading = true;
  try {
    p.mod = await import(`./optimize/${id}.js`);
    p.mod.mount(p.el, ctxFor(id));
    p.mounted = true;
  } catch (err) {
    failPane(p, err);
  } finally {
    p.loading = false;
  }
  if (p.mounted && visible && tab === id) callShow(id);
}

function callShow(id) {
  try {
    panes[id].mod?.onShow?.();
  } catch (err) {
    console.error(`[orbit] optimize/${id}.onShow failed`, err);
  }
}

function callHide(id) {
  try {
    panes[id].mod?.onHide?.();
  } catch (err) {
    console.error(`[orbit] optimize/${id}.onHide failed`, err);
  }
}

function render() {
  const t = TABS.find((x) => x.id === tab) || TABS[0];
  els.micro.textContent = `Optimize / ${t.label}`;
  els.title.innerHTML = t.title;
  for (const btn of els.tabs.querySelectorAll('[data-tab]')) {
    const on = btn.dataset.tab === tab;
    btn.setAttribute('aria-selected', on ? 'true' : 'false');
    btn.tabIndex = on ? 0 : -1;
  }
  for (const [id, p] of Object.entries(panes)) p.el.hidden = id !== tab;
}

function selectTab(id, focus = false) {
  if (!TABS.some((x) => x.id === id)) return;
  const prev = tab;
  tab = id;
  saveTab();
  if (root) {
    render();
    if (visible && prev !== id) {
      if (panes[prev]?.mounted) callHide(prev);
      if (panes[id].mounted) callShow(id);
      else ensureMounted(id);
    }
    if (focus) els.tabs.querySelector(`[data-tab="${id}"]`)?.focus();
  }
}

/** Show Optimize on sub-tab `id` ('health' | 'cleanup' | 'boost' | 'drivers'). */
export function openTab(id) {
  selectTab(id);
  if (currentViewName() !== 'optimize') navigate('optimize');
}

export function mount(rootEl) {
  root = rootEl;
  root.innerHTML = `
  <div class="op">
    <header class="view-head">
      <div>
        <p class="micro" data-op-micro>Optimize</p>
        <h1 class="view-title" data-op-title></h1>
      </div>
    </header>
    <div class="ar-tabs op-tabs" role="tablist" aria-label="Optimize">
      ${TABS.map((t) => `<button type="button" class="ar-tab" role="tab" id="op-tab-${t.id}" data-tab="${t.id}" aria-selected="false" aria-controls="op-pane-${t.id}" tabindex="-1">${t.label}<span class="ar-tab-count"></span></button>`).join('')}
    </div>
    ${TABS.map((t) => `<div class="op-pane" id="op-pane-${t.id}" role="tabpanel" aria-labelledby="op-tab-${t.id}" data-pane="${t.id}" hidden></div>`).join('')}
  </div>`;
  els.micro = root.querySelector('[data-op-micro]');
  els.title = root.querySelector('[data-op-title]');
  els.tabs = root.querySelector('.op-tabs');
  for (const t of TABS) panes[t.id] = { el: root.querySelector(`[data-pane="${t.id}"]`), mod: null, mounted: false, loading: false, failed: false };

  els.tabs.addEventListener('click', (ev) => {
    const btn = ev.target.closest('[data-tab]');
    if (btn) selectTab(btn.dataset.tab);
  });
  els.tabs.addEventListener('keydown', (ev) => {
    const i = TABS.findIndex((t) => t.id === tab);
    let next = -1;
    if (ev.key === 'ArrowRight') next = (i + 1) % TABS.length;
    else if (ev.key === 'ArrowLeft') next = (i - 1 + TABS.length) % TABS.length;
    else if (ev.key === 'Home') next = 0;
    else if (ev.key === 'End') next = TABS.length - 1;
    if (next < 0) return;
    ev.preventDefault();
    selectTab(TABS[next].id, true);
  });

  registerCommands(() => TABS.map((t) => ({
    label: `Optimize: ${t.label}`,
    search: `optimize ${t.label} ${t.search}`,
    icon: 'i-spark',
    run: () => openTab(t.id),
  })));

  render();
}

export function onShow() {
  visible = true;
  render();
  if (panes[tab].mounted) callShow(tab);
  else ensureMounted(tab);
}

export function onHide() {
  visible = false;
  if (panes[tab]?.mounted) callHide(tab);
}
