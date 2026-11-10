// The LegacyTech mark in the sidebar (roadmap 0.7, item 9a). Markup and look are in
// index.html (#sidebar-logo) and app.css (.sidebar-logo, .lt-*); this module only decides
// when the cyan signal pixel pulses and how fast.
//
// - The blocks assemble once from first paint (CSS .is-assembling); init() drops that class
//   afterwards, so showing the mark again (a taller window) does not replay it.
// - The pulse (.is-pulsing) runs only while motion.js motionOn() is true: the Animations
//   switch is on, Windows allows animations, and the window shows (not in the tray or
//   minimised). Otherwise there is no animation at all.
// - Each snapshot sets the pulse's playbackRate from CPU load (calm at idle, about three
//   times faster at 100%). playbackRate keeps the current phase, so a change never jumps,
//   and it touches no layout.

import { motionOn } from './motion.js';
import { on } from './store.js';

const ASSEMBLE_MS = 1500; // the CSS assembly ends by about 1.04 s
const CALM = 1; // one breath per 3.2 s (app.css lt-pulse)
const BUSY = 3; // at 100% CPU, about one a second
const STEP = 0.25; // rates move in quarter steps, so small CPU wobbles change nothing

let logo = null;
let sig = null;
let anim = null; // the running lt-pulse CSSAnimation, once looked up
let rate = CALM;

/** Pulse rate for a CPU load in percent. */
export function pulseRate(cpu) {
  const k = Math.min(1, Math.max(0, Number(cpu) / 100)) || 0;
  return Math.round((CALM + (BUSY - CALM) * k) / STEP) * STEP;
}

function sync() {
  if (!logo) return;
  const live = motionOn();
  if (logo.classList.contains('is-pulsing') === live) return;
  logo.classList.toggle('is-pulsing', live);
  anim = null; // a new CSS animation starts at rate 1 (or none runs)
  rate = CALM;
}

/** Follow CPU load from a snapshot. Called once per tick; cheap when nothing changes. */
export function logoTick(snap) {
  if (!logo || !logo.classList.contains('is-pulsing') || document.hidden) return;
  const cpu = snap && snap.cpu ? snap.cpu.usage : null;
  if (typeof cpu !== 'number') return;
  if (anim && anim.playState === 'idle') { // CSS restarted it (the mark was hidden by height)
    anim = null;
    rate = CALM;
  }
  const next = pulseRate(cpu);
  if (next === rate) return;
  if (!anim) anim = sig.getAnimations()[0] || null;
  if (!anim) return; // not drawn right now
  rate = next;
  if (typeof anim.updatePlaybackRate === 'function') anim.updatePlaybackRate(next);
  else anim.playbackRate = next;
}

/** Wire the mark: pulse on or off with settings, Windows' setting and window visibility. */
export function initLogo() {
  logo = document.getElementById('sidebar-logo');
  sig = logo ? logo.querySelector('.lt-sig') : null;
  if (!logo || !sig) {
    logo = null;
    return;
  }
  setTimeout(() => logo.classList.remove('is-assembling'), ASSEMBLE_MS);
  sync();
  on('settings', sync);
  document.addEventListener('visibilitychange', sync);
  const query = typeof window.matchMedia === 'function' ? window.matchMedia('(prefers-reduced-motion: reduce)') : null;
  if (query && typeof query.addEventListener === 'function') query.addEventListener('change', sync);
}
