// Orbit / layouts: text typed on the wrong keyboard layout. `keyboard_layouts` returns,
// for each pair of installed layouts, the characters that differ:
// [{ from, to, map: { "ؤ": "c", ... } }]. Pure functions, no imports, so the smoke test can
// load this module on its own.

/** `text` re-typed through `map`: each character the map knows is swapped, others kept. */
export function convertLayout(text, map) {
  if (typeof text !== 'string' || !map || typeof map !== 'object') return String(text ?? '');
  let out = '';
  for (const ch of text) out += Object.prototype.hasOwnProperty.call(map, ch) ? map[ch] : ch;
  return out;
}

/**
 * The query as it would read on each other layout, most likely first: only maps that know
 * every non-space character of `text` (so plain English is not "converted" into noise),
 * without duplicates or the text itself.
 */
export function layoutVariants(text, maps) {
  const q = String(text || '');
  if (!q.trim() || !Array.isArray(maps)) return [];
  const chars = [...q].filter((c) => c.trim());
  const seen = new Set([q.toLowerCase()]);
  const out = [];
  for (const m of maps) {
    if (!m || !m.map) continue;
    if (!chars.every((c) => Object.prototype.hasOwnProperty.call(m.map, c))) continue;
    const v = convertLayout(q, m.map);
    const key = v.toLowerCase();
    if (seen.has(key)) continue;
    seen.add(key);
    out.push(v);
  }
  return out;
}
