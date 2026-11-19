// Orbit / Biggest folders disk map: squarified treemap layout (pure) and the boxes (DOM).
// Boxes are real buttons, so Tab, Enter, tooltips, theme colours and sharp text on any screen
// density come from the browser. Read only: nothing here changes the PC.
import { escapeHtml, bytes } from '../../format.js';

// Squarified treemap (Bruls, Huijsen, van Wijk). items: [{ value, ...}] with value > 0, any order.
// Returns [{ item, x, y, w, h }] filling the w x h rectangle, biggest first.
export function squarify(items, w, h) {
  const list = items.filter((i) => i.value > 0).sort((a, b) => b.value - a.value);
  const total = list.reduce((a, i) => a + i.value, 0);
  const out = [];
  if (!list.length || w <= 0 || h <= 0) return out;
  const scale = (w * h) / total;
  let x = 0;
  let y = 0;
  let rw = w;
  let rh = h;
  let row = [];
  const worst = (r, side) => {
    const sum = r.reduce((a, i) => a + i.area, 0);
    const max = Math.max(...r.map((i) => i.area));
    const min = Math.min(...r.map((i) => i.area));
    return Math.max((side * side * max) / (sum * sum), (sum * sum) / (side * side * min));
  };
  const flush = () => {
    const sum = row.reduce((a, i) => a + i.area, 0);
    if (rw >= rh) {
      const cw = sum / rh;
      let cy = y;
      for (const i of row) {
        const ch = i.area / cw;
        out.push({ item: i.item, x, y: cy, w: cw, h: ch });
        cy += ch;
      }
      x += cw;
      rw -= cw;
    } else {
      const ch = sum / rw;
      let cx = x;
      for (const i of row) {
        const cw = i.area / ch;
        out.push({ item: i.item, x: cx, y, w: cw, h: ch });
        cx += cw;
      }
      y += ch;
      rh -= ch;
    }
    row = [];
  };
  for (const item of list) {
    const cell = { item, area: item.value * scale };
    const side = Math.min(rw, rh);
    if (!row.length || worst([...row, cell], side) <= worst(row, side)) row.push(cell);
    else {
      flush();
      row.push(cell);
    }
  }
  if (row.length) flush();
  return out;
}

// What the map shows for one FolderSizes result.
export function mapItems(d) {
  const items = d.folders.map((f) => ({ kind: 'folder', name: f.name, path: f.path, value: f.bytes }));
  if (d.otherBytes > 0) items.push({ kind: 'other', name: 'Other folders', value: d.otherBytes, note: `${Number(d.otherCount || 0).toLocaleString()} smaller folders` });
  if (d.filesHereBytes > 0) items.push({ kind: 'files', name: 'Files in this folder', value: d.filesHereBytes, note: `${Number(d.filesHere || 0).toLocaleString()} files` });
  return items;
}

const GAP = 2;

// Fills `el` (a positioned box with a height) with one button per item.
// handlers: { drill(path), up(), open(path), selected(box|null) }. Returns { render(d), destroy() }.
export function mountMap(el, handlers) {
  let data = null;
  let selected = null;
  const ro = new ResizeObserver(() => draw());

  const boxes = () => [...el.querySelectorAll('.cl-tm-box')];

  function select(box) {
    selected = box ? box.dataset.path || box.dataset.kind : null;
    handlers.selected?.(box ? { kind: box.dataset.kind, path: box.dataset.path || '', name: box.dataset.name, title: box.title } : null);
  }

  function draw() {
    if (!data) return;
    const items = mapItems(data);
    if (!items.length) {
      el.innerHTML = '';
      return;
    }
    const cells = squarify(items, el.clientWidth, el.clientHeight);
    const total = items.reduce((a, i) => a + i.value, 0);
    el.innerHTML = cells.map((c, n) => {
      const it = c.item;
      const w = Math.max(0, c.w - GAP);
      const h = Math.max(0, c.h - GAP);
      const share = Math.round((it.value / total) * 100);
      const tip = `${it.name}: ${bytes(it.value)}${it.note ? ` (${it.note})` : ''}${it.kind === 'folder' ? '. Click to look inside.' : ''}`;
      const label = w >= 64 && h >= 34
        ? `<span class="cl-tm-name">${escapeHtml(it.name)}</span>${h >= 52 ? `<span class="cl-tm-size">${escapeHtml(bytes(it.value))}</span>` : ''}`
        : '';
      return `<button type="button" class="cl-tm-box cl-tm-box--${it.kind}" data-kind="${it.kind}" data-name="${escapeHtml(it.name)}"${it.path ? ` data-path="${escapeHtml(it.path)}"` : ''} data-rank="${Math.min(n, 5)}" style="left:${(c.x + GAP / 2).toFixed(1)}px;top:${(c.y + GAP / 2).toFixed(1)}px;width:${w.toFixed(1)}px;height:${h.toFixed(1)}px" title="${escapeHtml(tip)}" aria-label="${escapeHtml(`${it.name}, ${bytes(it.value)}, ${share}% of this view`)}">${label}</button>`;
    }).join('');
    if (!selected || !boxes().some((b) => (b.dataset.path || b.dataset.kind) === selected)) select(null);
  }

  function move(from, key) {
    const r = from.getBoundingClientRect();
    const cx = r.left + r.width / 2;
    const cy = r.top + r.height / 2;
    const horizontal = key === 'ArrowLeft' || key === 'ArrowRight';
    let best = null;
    let bestScore = Infinity;
    for (const b of boxes()) {
      if (b === from) continue;
      const q = b.getBoundingClientRect();
      const dx = q.left + q.width / 2 - cx;
      const dy = q.top + q.height / 2 - cy;
      const ok = key === 'ArrowRight' ? dx > 1 : key === 'ArrowLeft' ? dx < -1 : key === 'ArrowDown' ? dy > 1 : dy < -1;
      if (!ok) continue;
      const score = horizontal ? Math.abs(dx) + Math.abs(dy) * 2 : Math.abs(dy) + Math.abs(dx) * 2;
      if (score < bestScore) {
        best = b;
        bestScore = score;
      }
    }
    best?.focus();
  }

  el.addEventListener('click', (ev) => {
    const b = ev.target.closest('.cl-tm-box');
    if (b?.dataset.path) handlers.drill(b.dataset.path);
  });
  el.addEventListener('contextmenu', (ev) => {
    ev.preventDefault();
    handlers.up();
  });
  el.addEventListener('keydown', (ev) => {
    if (ev.key === 'Backspace') {
      ev.preventDefault();
      handlers.up();
      return;
    }
    const b = ev.target.closest?.('.cl-tm-box');
    if (!b) return;
    if (ev.key.startsWith('Arrow')) {
      ev.preventDefault();
      move(b, ev.key);
    } else if ((ev.key === 'o' || ev.key === 'O') && b.dataset.path) {
      ev.preventDefault();
      handlers.open(b.dataset.path);
    }
  });
  const pick = (ev) => {
    const b = ev.target.closest?.('.cl-tm-box');
    if (b) select(b);
  };
  el.addEventListener('focusin', pick);
  el.addEventListener('mouseover', pick);
  ro.observe(el);

  return {
    render(d) {
      data = d;
      selected = null;
      draw();
    },
    destroy() {
      ro.disconnect();
    },
  };
}
