// Which Windows services each process hosts (Processes rows and the details drawer).
// Not part of the snapshot: one small command, cached here and refreshed about every 10 s
// while a view that shows it is visible. Emits 'hosted' when the data changes.
import { call } from './api.js';
import { emit } from './store.js';

const REFRESH_MS = 10000;

let map = new Map(); // pid -> [{ name, displayName }]
let key = '';
let timer = 0;
let loading = false;
let lastAt = 0;

/** Services hosted by this PID, or an empty list. */
export function hostedServices(pid) {
  return map.get(pid) || [];
}

/** "Windows Audio, Audio Endpoint Builder" for a row's secondary text, or ''. */
export function hostedSummary(pid) {
  return hostedServices(pid).map((s) => s.displayName || s.name).join(', ');
}

export async function refreshHosted(force = false) {
  if (loading || (!force && Date.now() - lastAt < REFRESH_MS / 2)) return;
  loading = true;
  try {
    const raw = await call('process_services');
    const next = new Map();
    for (const [pid, list] of Object.entries(raw || {})) {
      if (Array.isArray(list) && list.length) next.set(Number(pid), list);
    }
    const nextKey = JSON.stringify([...next]);
    if (nextKey !== key) {
      key = nextKey;
      map = next;
      emit('hosted', null);
    }
  } catch (_) { /* keep the last map; the row just shows no services */ }
  loading = false;
  lastAt = Date.now();
}

export function startHosted() {
  refreshHosted();
  if (!timer) {
    timer = setInterval(() => { if (!document.hidden) refreshHosted(true); }, REFRESH_MS);
  }
}

export function stopHosted() {
  if (timer) clearInterval(timer);
  timer = 0;
}
