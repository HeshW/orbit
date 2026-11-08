// Orbit / charts: crisp, DPR-aware canvas line charts and sparklines.
// Colours are CSS variable names (e.g. '--chart-cpu') or plain CSS colours. They are read
// with getComputedStyle at draw time and cached until the theme changes.

const DEFAULT_POINTS = 60;

// ---------------------------------------------------------------- colour resolution

let themeVersion = 0;
let cacheVersion = -1;
const colorCache = new Map();

if (typeof MutationObserver !== 'undefined') {
  new MutationObserver(() => { themeVersion++; }).observe(document.documentElement, {
    attributes: true,
    attributeFilter: ['data-theme', 'class', 'style'],
  });
}

let probeCtx = null;
function probe() {
  if (!probeCtx) {
    const c = document.createElement('canvas');
    c.width = 1;
    c.height = 1;
    probeCtx = c.getContext('2d');
  }
  return probeCtx;
}

/** Resolve a CSS var name or colour into { css, r, g, b, a }, cached per theme. */
function resolveColor(name) {
  if (cacheVersion !== themeVersion) {
    colorCache.clear();
    cacheVersion = themeVersion;
  }
  const key = name || '--chart-cpu';
  let c = colorCache.get(key);
  if (c) return c;

  let value = key;
  if (key.startsWith('--')) {
    value = getComputedStyle(document.documentElement).getPropertyValue(key).trim() || '#38d5f0';
  }
  const ctx = probe();
  ctx.fillStyle = '#000000';
  ctx.fillStyle = value;
  const norm = String(ctx.fillStyle);
  let r = 0;
  let g = 0;
  let b = 0;
  let a = 1;
  if (norm[0] === '#') {
    r = parseInt(norm.slice(1, 3), 16);
    g = parseInt(norm.slice(3, 5), 16);
    b = parseInt(norm.slice(5, 7), 16);
  } else {
    const m = norm.match(/rgba?\(([^)]+)\)/);
    if (m) {
      const parts = m[1].split(',').map((p) => parseFloat(p));
      r = parts[0] || 0;
      g = parts[1] || 0;
      b = parts[2] || 0;
      a = parts.length > 3 && Number.isFinite(parts[3]) ? parts[3] : 1;
    }
  }
  c = {
    css: norm,
    r, g, b, a,
    rgba(alpha) { return `rgba(${r}, ${g}, ${b}, ${alpha * a})`; },
  };
  colorCache.set(key, c);
  return c;
}

function num(v) {
  return typeof v === 'number' && Number.isFinite(v) ? v : 0;
}

function seriesMax(seriesData) {
  let m = 0;
  for (let s = 0; s < seriesData.length; s++) {
    const d = seriesData[s];
    if (!d) continue;
    for (let i = 0; i < d.length; i++) {
      const v = d[i];
      if (typeof v === 'number' && v > m) m = v;
    }
  }
  return m;
}

function dpr() {
  return Math.max(1, Math.min(3, window.devicePixelRatio || 1));
}

/**
 * Size a canvas backing store to its CSS box (w, h in CSS px) at the current device pixel
 * ratio. Sizes come from ResizeObserver, so drawing never forces a layout.
 */
function fit(canvas, state, w = state.w, h = state.h) {
  const ratio = dpr();
  state.w = w;
  state.h = h;
  state.ratio = ratio;
  const bw = Math.max(1, Math.round(w * ratio));
  const bh = Math.max(1, Math.round(h * ratio));
  if (canvas.width !== bw || canvas.height !== bh) {
    canvas.width = bw;
    canvas.height = bh;
    return true;
  }
  return false;
}

function boxOf(entry) {
  const b = entry.contentBoxSize && entry.contentBoxSize[0];
  if (b) return [b.inlineSize, b.blockSize];
  return [entry.contentRect.width, entry.contentRect.height];
}

/** Trace a series as a polyline, newest point on the right edge. */
function tracePath(ctx, data, points, w, h, max, offset) {
  const n = data.length;
  const start = Math.max(0, n - points);
  const step = points > 1 ? w / (points - 1) : w;
  const top = 1 + offset;
  const usable = h - top - 1;
  let firstX = 0;
  let lastX = 0;
  for (let i = start; i < n; i++) {
    let v = num(data[i]) / max;
    if (v < 0) v = 0;
    else if (v > 1) v = 1;
    const x = w - (n - 1 - i) * step;
    const y = top + (1 - v) * usable;
    if (i === start) {
      ctx.moveTo(x, y);
      firstX = x;
    } else {
      ctx.lineTo(x, y);
    }
    lastX = x;
  }
  return n - start > 0 ? [firstX, lastX] : null;
}

// ---------------------------------------------------------------- LineChart

export class LineChart {
  /**
   * @param {HTMLCanvasElement} canvas
   * @param {{ series?: {color: string, fill?: boolean}[], max?: number|'auto', grid?: boolean,
   *           points?: number, lineWidth?: number }} opts
   */
  constructor(canvas, opts = {}) {
    this.canvas = canvas;
    this.ctx = canvas.getContext('2d');
    this.series = Array.isArray(opts.series) && opts.series.length ? opts.series : [{ color: '--chart-cpu', fill: true }];
    this.max = opts.max ?? 100;
    this.grid = opts.grid !== false;
    this.points = Math.max(2, opts.points || DEFAULT_POINTS);
    this.lineWidth = opts.lineWidth || 1.6;
    this.currentMax = typeof this.max === 'number' ? this.max : 1;
    this.data = null;
    this._state = { w: 0, h: 0, ratio: 1 };
    this._grads = [];
    this._gradKey = [];
    this._ro = null;
    if (typeof ResizeObserver !== 'undefined') {
      this._ro = new ResizeObserver((entries) => {
        const [w, h] = boxOf(entries[entries.length - 1]);
        fit(this.canvas, this._state, w, h);
        this._render();
      });
      this._ro.observe(canvas);
    } else {
      fit(canvas, this._state, canvas.clientWidth, canvas.clientHeight);
    }
  }

  /** Draw series data: number[][] in the same order as opts.series. */
  draw(seriesData) {
    this.data = seriesData || null;
    if (this._state.ratio !== dpr()) fit(this.canvas, this._state);
    this._render();
  }

  destroy() {
    if (this._ro) this._ro.disconnect();
    this._ro = null;
  }

  _gradient(i, color, h, ratio) {
    const key = `${color.css}|${h}|${ratio}|${themeVersion}`;
    if (this._gradKey[i] !== key) {
      const g = this.ctx.createLinearGradient(0, 0, 0, h);
      g.addColorStop(0, color.rgba(0.3));
      g.addColorStop(1, color.rgba(0));
      this._grads[i] = g;
      this._gradKey[i] = key;
    }
    return this._grads[i];
  }

  _render() {
    const { w, h, ratio } = this._state;
    const ctx = this.ctx;
    if (!ctx || w <= 0 || h <= 0) return;
    ctx.setTransform(1, 0, 0, 1, 0, 0);
    ctx.clearRect(0, 0, this.canvas.width, this.canvas.height);
    ctx.setTransform(ratio, 0, 0, ratio, 0, 0);

    if (this.grid) {
      const gridColor = resolveColor('--chart-grid');
      const px = 1 / ratio;
      ctx.beginPath();
      for (let i = 1; i < 4; i++) {
        const y = Math.round((h * i) / 4 * ratio) / ratio + px / 2;
        ctx.moveTo(0, y);
        ctx.lineTo(w, y);
      }
      for (let i = 1; i < 6; i++) {
        const x = Math.round((w * i) / 6 * ratio) / ratio + px / 2;
        ctx.moveTo(x, 0);
        ctx.lineTo(x, h);
      }
      ctx.strokeStyle = gridColor.css;
      ctx.lineWidth = px;
      ctx.stroke();
    }

    const data = this.data;
    if (!data || !data.length) return;

    let max = this.max;
    if (max === 'auto') {
      const peak = seriesMax(data);
      max = peak > 0 ? peak * 1.15 : 1;
    }
    if (!(max > 0)) max = 1;
    this.currentMax = max;

    const lw = this.lineWidth;
    ctx.lineJoin = 'round';
    ctx.lineCap = 'round';

    for (let s = 0; s < this.series.length; s++) {
      const d = data[s];
      if (!d || !d.length) continue;
      const spec = this.series[s];
      const color = resolveColor(spec.color);

      if (spec.fill !== false) {
        ctx.beginPath();
        const ends = tracePath(ctx, d, this.points, w, h, max, lw / 2);
        if (ends) {
          ctx.lineTo(ends[1], h);
          ctx.lineTo(ends[0], h);
          ctx.closePath();
          ctx.fillStyle = this._gradient(s, color, h, ratio);
          ctx.fill();
        }
      }

      ctx.beginPath();
      if (tracePath(ctx, d, this.points, w, h, max, lw / 2)) {
        ctx.strokeStyle = color.css;
        ctx.lineWidth = lw;
        ctx.stroke();
      }
    }
  }
}

// ---------------------------------------------------------------- sparkline

const sparkState = new WeakMap();
const sparkRO = typeof ResizeObserver !== 'undefined'
  ? new ResizeObserver((entries) => {
    for (const entry of entries) {
      const st = sparkState.get(entry.target);
      if (!st) continue;
      const [w, h] = boxOf(entry);
      st.w = w;
      st.h = h;
      st.sized = true;
      if (st.last) sparkline(entry.target, st.last.data, st.last.opts);
    }
  })
  : null;

/**
 * Small line + soft fill, newest value on the right. Sized from the canvas CSS box.
 * @param {HTMLCanvasElement} canvas
 * @param {number[]} data
 * @param {{ color?: string, max?: number|'auto', points?: number, fill?: boolean }} opts
 */
export function sparkline(canvas, data, opts = {}) {
  if (!canvas) return;
  let st = sparkState.get(canvas);
  if (!st) {
    st = { w: 0, h: 0, ratio: 1, ctx: canvas.getContext('2d'), grad: null, gradKey: '', sized: false, last: null };
    sparkState.set(canvas, st);
    // First call measures once; the observer keeps the size current afterwards.
    st.w = canvas.clientWidth;
    st.h = canvas.clientHeight;
    if (sparkRO) sparkRO.observe(canvas);
  }
  st.last = st.last || { data: null, opts: null };
  st.last.data = data;
  st.last.opts = opts;
  fit(canvas, st);
  const { w, h, ratio, ctx } = st;
  if (!ctx || w <= 0 || h <= 0) return;

  ctx.setTransform(1, 0, 0, 1, 0, 0);
  ctx.clearRect(0, 0, canvas.width, canvas.height);
  if (!data || !data.length) return;
  ctx.setTransform(ratio, 0, 0, ratio, 0, 0);

  const color = resolveColor(opts.color || '--chart-cpu');
  let max = opts.max ?? 100;
  if (max === 'auto') {
    let peak = 0;
    for (let i = 0; i < data.length; i++) if (data[i] > peak) peak = data[i];
    max = peak > 0 ? peak * 1.15 : 1;
  }
  if (!(max > 0)) max = 1;
  const points = Math.max(2, opts.points || DEFAULT_POINTS);
  const lw = 1.25;

  if (opts.fill !== false) {
    const key = `${color.css}|${h}|${ratio}|${themeVersion}`;
    if (st.gradKey !== key) {
      const g = ctx.createLinearGradient(0, 0, 0, h);
      g.addColorStop(0, color.rgba(0.28));
      g.addColorStop(1, color.rgba(0));
      st.grad = g;
      st.gradKey = key;
    }
    ctx.beginPath();
    const ends = tracePath(ctx, data, points, w, h, max, lw / 2);
    if (ends) {
      ctx.lineTo(ends[1], h);
      ctx.lineTo(ends[0], h);
      ctx.closePath();
      ctx.fillStyle = st.grad;
      ctx.fill();
    }
  }

  ctx.beginPath();
  if (tracePath(ctx, data, points, w, h, max, lw / 2)) {
    ctx.lineJoin = 'round';
    ctx.lineCap = 'round';
    ctx.strokeStyle = color.css;
    ctx.lineWidth = lw;
    ctx.stroke();
  }
}
