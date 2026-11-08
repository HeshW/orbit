// Orbit / format: number, byte, rate, time and text helpers shared by every view.

const DASH = '–';
const UNITS = ['B', 'KB', 'MB', 'GB', 'TB', 'PB'];
const MONTHS = ['Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun', 'Jul', 'Aug', 'Sep', 'Oct', 'Nov', 'Dec'];

function isNum(n) {
  return typeof n === 'number' && Number.isFinite(n);
}

function pad2(n) {
  return n < 10 ? `0${n}` : String(n);
}

/** "512 KB", "1.4 GB" (1024 base). Values of 100 and up drop the decimals. */
export function bytes(n, digits = 1) {
  if (!isNum(n)) return DASH;
  const neg = n < 0;
  let v = Math.abs(n);
  if (v < 1024) return `${neg ? '-' : ''}${Math.round(v)} B`;
  let i = 0;
  while (v >= 1024 && i < UNITS.length - 1) {
    v /= 1024;
    i++;
  }
  // Rounding can push 1023.96 up to "1024": step to the next unit instead.
  if (v >= 1023.95 && i < UNITS.length - 1) {
    v /= 1024;
    i++;
  }
  const s = v >= 100 ? v.toFixed(0) : v.toFixed(digits);
  return `${neg ? '-' : ''}${s} ${UNITS[i]}`;
}

/** "3.2 MB/s", "0 B/s". */
export function rate(bps) {
  if (!isNum(bps)) return DASH;
  if (bps <= 0) return '0 B/s';
  return `${bytes(bps)}/s`;
}

/** "12.5%". */
export function pct(n, digits = 1) {
  if (!isNum(n)) return DASH;
  return `${n.toFixed(digits)}%`;
}

/** "3d 4h", "2h 05m", "12m 30s", "45s". */
export function duration(secs) {
  if (!isNum(secs) || secs < 0) return DASH;
  const t = Math.floor(secs);
  const d = Math.floor(t / 86400);
  const h = Math.floor((t % 86400) / 3600);
  const m = Math.floor((t % 3600) / 60);
  const s = t % 60;
  if (d > 0) return `${d}d ${h}h`;
  if (h > 0) return `${h}h ${pad2(m)}m`;
  if (m > 0) return `${m}m ${pad2(s)}s`;
  return `${s}s`;
}

/** Local "14:03, 8 Nov". */
export function time(epochSecs) {
  if (!isNum(epochSecs) || epochSecs <= 0) return DASH;
  const dt = new Date(epochSecs * 1000);
  return `${pad2(dt.getHours())}:${pad2(dt.getMinutes())}, ${dt.getDate()} ${MONTHS[dt.getMonth()]}`;
}

/** Tooltip for per-process "I/O" rates (GetProcessIoCounters counts every kind of I/O). */
export const IO_TITLE = 'Read + write rate of all I/O (files, pipes, network)';

/**
 * Time covered by a 60-sample history at this update interval:
 * "30 s" / "5 min", or "30 seconds" / "5 minutes" with long = true.
 */
export function historySpan(intervalMs, long = false) {
  const ms = isNum(intervalMs) && intervalMs > 0 ? intervalMs : 1000;
  const secs = Math.round((60 * ms) / 1000);
  if (secs < 120) return long ? `${secs} seconds` : `${secs} s`;
  const mins = Math.round(secs / 60);
  return long ? `${mins} minutes` : `${mins} min`;
}

/** 0..1 for heat tints. */
export function heat(value, max) {
  if (!isNum(value) || !isNum(max) || max <= 0) return 0;
  const h = value / max;
  return h <= 0 ? 0 : h >= 1 ? 1 : h;
}

const ESC = { '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' };
const ESC_RE = /[&<>"']/g;

export function escapeHtml(s) {
  if (s === null || s === undefined) return '';
  return String(s).replace(ESC_RE, (c) => ESC[c]);
}

function isBoundary(ch) {
  return ch === ' ' || ch === '\\' || ch === '/' || ch === '.' || ch === '_' || ch === '-' || ch === '(' || ch === ':';
}

/**
 * Case-insensitive match. Returns a score (higher is better) or -1 for no match.
 * An empty needle matches everything with score 0.
 * Substring matches score 1000+ (earlier and on a word boundary score higher);
 * in-order character matches (spaces in the needle ignored) score below 1000.
 */
export function fuzzy(haystack, needle) {
  if (needle === null || needle === undefined) return 0;
  const n = String(needle).trim().toLowerCase();
  if (!n) return 0;
  if (haystack === null || haystack === undefined) return -1;
  const h = String(haystack).toLowerCase();
  if (!h) return -1;

  const idx = h.indexOf(n);
  if (idx !== -1) {
    let score = 2000 - Math.min(idx, 900);
    if (idx === 0) score += 400;
    else if (isBoundary(h[idx - 1])) score += 200;
    if (h.length === n.length) score += 400;
    score -= Math.min(200, (h.length - n.length) * 0.5);
    return score;
  }

  // In-order characters.
  let hi = 0;
  let score = 600;
  let prev = -2;
  let matched = 0;
  for (let ni = 0; ni < n.length; ni++) {
    const ch = n[ni];
    if (ch === ' ') continue;
    const found = h.indexOf(ch, hi);
    if (found === -1) return -1;
    if (found === prev + 1) score += 12;
    else score -= Math.min(30, found - hi);
    if (found === 0 || isBoundary(h[found - 1])) score += 8;
    prev = found;
    hi = found + 1;
    matched++;
  }
  if (!matched) return 0;
  score -= Math.min(150, h.length * 0.25);
  return Math.max(1, Math.min(999, score));
}
