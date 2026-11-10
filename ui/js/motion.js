// Motion in the legacytech.me style (roadmap 0.6, item 8d): a view's main blocks fade and
// rise in when you switch to it, and its headline numbers count up the first time it shows.
//
// One switch turns all of it off: Settings > Display > Animations (settings.reducedMotion,
// which puts .reduce-motion on <html>). Windows' "Show animations" off (prefers-reduced-motion)
// does the same. A hidden window (tray, minimised) never starts any of it. The hover motion
// (lift, sweep, arrows, underlines) is plain CSS in app.css and stops under the same switch.

const ENTER_SEL = '.view-head, .toolbar, .panel, .pass, .op-tabs, .perf-tiles, .proc-panel, .empty';
const ENTER_STEPS = 6; // blocks after the sixth start with it, so a long page never waits
const COUNT_SEL = '.stat-value, .perf-readout__value, .perf-tile__value, .hl-summary-chips .chip, .cl-foot-total strong';
const COUNT_MS = 450;
const COUNT_WINDOW_MS = 6000; // numbers that first arrive later than this just appear
const NUM = /\d[\d,]*(?:\.\d+)?/g;

const reduceQuery = typeof window.matchMedia === 'function' ? window.matchMedia('(prefers-reduced-motion: reduce)') : null;

/** True when animations may run now: the switch is on, Windows allows them, the window shows. */
export function motionOn() {
  return !document.documentElement.classList.contains('reduce-motion')
    && !(reduceQuery && reduceQuery.matches)
    && !document.hidden;
}

// ------------------------------------------------------------------ view entrance

function entered(e) {
  if (e.animationName !== 'orbit-enter') return;
  e.target.classList.remove('enter-up');
  e.target.style.removeProperty('--enter-i');
}

/** Fade and rise the view's top-level blocks in, 70 ms apart (CSS .enter-up). */
export function enterView(view) {
  if (!view) return;
  for (const el of view.querySelectorAll('.enter-up')) {
    el.classList.remove('enter-up');
    el.style.removeProperty('--enter-i');
  }
  if (!motionOn()) return;
  if (!view.__enterBound) {
    view.addEventListener('animationend', entered);
    view.addEventListener('animationcancel', entered);
    view.__enterBound = true;
  }
  let i = 0;
  for (const el of view.querySelectorAll(ENTER_SEL)) {
    if (el.parentElement && el.parentElement.closest(ENTER_SEL)) continue; // inside a block
    if (!el.getClientRects().length) continue; // hidden (another tab's pane)
    el.style.setProperty('--enter-i', String(Math.min(i, ENTER_STEPS - 1)));
    el.classList.add('enter-up');
    i++;
  }
}

/** A block that appears after its view has shown (new data, such as the Sensors block when
 *  a reading arrives): the same fade and rise, by itself. */
export function enterBlock(el) {
  if (!el || !motionOn()) return;
  if (!el.__enterBound) {
    el.addEventListener('animationend', entered);
    el.addEventListener('animationcancel', entered);
    el.__enterBound = true;
  }
  el.classList.remove('enter-up');
  el.style.setProperty('--enter-i', '0');
  void el.offsetWidth; // restart the animation if it was mid-way
  el.classList.add('enter-up');
}

// ------------------------------------------------------------------ count-up

/** Text that reads as a number to count: "45%", "32 GB", "14 / 20", "1.5 TB", "3 good". */
export function countable(text) {
  const t = String(text || '').trim();
  if (!/^\d/.test(t) || t.includes(':')) return false; // labels, times
  if (/\d[a-z]/i.test(t)) return false; // "3h 20m": durations read wrong mid-count
  return /[1-9]/.test(t);
}

function numText(v, dp, comma) {
  const s = v.toFixed(dp);
  if (!comma) return s;
  const [int, frac] = s.split('.');
  return int.replace(/\B(?=(\d{3})+(?!\d))/g, ',') + (frac ? `.${frac}` : '');
}

/** Count every number in el's text up from zero, ending on the exact text. Stops as soon as
 *  the view writes a newer value (a live tick), so it never fights the data. */
export function countUp(el, ms = COUNT_MS) {
  if (!el || !motionOn() || el.closest('[aria-live]')) return;
  const final = el.textContent;
  if (!countable(final)) return;
  const parts = [...final.matchAll(NUM)].map((m) => ({
    v: parseFloat(m[0].replace(/,/g, '')),
    dp: (m[0].split('.')[1] || '').length,
    comma: m[0].includes(','),
  }));
  const t0 = performance.now();
  let last = null;
  const frame = (now) => {
    if (!el.isConnected || (last !== null && el.textContent !== last)) return;
    const p = Math.min(1, Math.max(0, (now - t0) / ms));
    if (p >= 1 || !motionOn()) {
      el.textContent = final;
      return;
    }
    const k = 1 - (1 - p) ** 3; // ease-out, close to the design system's curve
    let j = 0;
    last = final.replace(NUM, () => {
      const q = parts[j++];
      return numText(q.v * k, q.dp, q.comma);
    });
    el.textContent = last;
    requestAnimationFrame(frame);
  };
  frame(t0);
}

// Same element slot across re-renders (innerHTML), so a number counts once per view.
function slot(view, el) {
  const all = view.querySelectorAll(COUNT_SEL);
  return `${Array.prototype.indexOf.call(all, el)}|${el.className}`;
}

/** First show of a view: count up its headline numbers as they first appear (now, or when
 *  the data lands within a few seconds). Returns a stop function for when the view hides. */
export function countIn(view) {
  if (!view || !motionOn()) return () => {};
  const done = new Set();
  const seen = new WeakSet();
  const visit = (el) => {
    if (seen.has(el) || !countable(el.textContent)) return;
    seen.add(el);
    const key = slot(view, el);
    if (done.has(key)) return;
    done.add(key);
    countUp(el);
  };
  const scan = (node) => {
    if (!(node instanceof Element)) return;
    if (node.matches(COUNT_SEL)) visit(node);
    for (const el of node.querySelectorAll(COUNT_SEL)) visit(el);
  };
  scan(view);
  const obs = new MutationObserver((records) => {
    for (const r of records) {
      const target = r.target.nodeType === 3 ? r.target.parentElement : r.target;
      const hit = target && target.closest(COUNT_SEL);
      if (hit && view.contains(hit)) visit(hit);
      for (const n of r.addedNodes) scan(n);
    }
  });
  obs.observe(view, { childList: true, subtree: true, characterData: true });
  const timer = setTimeout(() => obs.disconnect(), COUNT_WINDOW_MS);
  return () => {
    clearTimeout(timer);
    obs.disconnect();
  };
}
