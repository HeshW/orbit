# Feature: LegacyTech logo in the sidebar (v0.7, 9a)

The LegacyTech pixel L sits in the free space under Settings, inside a thin ring. When Orbit
opens, its blocks drop in one by one; then the cyan signal pixel breathes slowly, faster when
the CPU is busy. Clicking it opens Settings at About. UI only: no commands, events or new
settings. Shapes are in `docs/CONTRACT.md` ("v0.7").

Files: `ui/index.html` (`#sidebar-logo`), `ui/css/app.css` (`.sidebar-logo*`, `.lt-*`),
`ui/css/tokens.css` (`--logo-ink/-blue/-cyan`), `ui/js/logo.js` (new), `ui/js/app.js`,
`tests/ui-smoke.mjs`.

## Look

- The mark from `assets/legacytech-logo-kit/mark` at the kit's proportions (block 20, gap 5):
  8px blocks with 2px gaps, 38px square, built from seven `<span>`s in a CSS grid. No image.
- Colours from the kit: color-dark (`#f2f5ff`, `#4f8cff`, `#38d5f0`) in Night, Void and Signal;
  color-light (`#0a0f1f`, `#2f6feb`, `#0b8db8`) in Daylight. Signal keeps the kit colours rather
  than its brighter accents (the kit says not to recolour the mark).
- A 76px ring, 1px, `--blue-soft` fading from 50% to nothing across the diagonal. It brightens
  a little on hover.
- The slot between the nav and the Live / System card takes all the free height and centres
  the mark halfway between them. When that would leave less than about 18px on each side
  (window height below roughly 765px; the default 1320 x 840 window leaves about 55px) the
  mark is not drawn, so it never overlaps the nav or the card, and it is out of the Tab order.
  An empty slot leaves the layout exactly as in 0.6.

## Motion

| When | What |
|---|---|
| First paint | Blocks fade and drop in, top to bottom then along the foot: 0.38 s each, 110 ms apart, done by about 1.04 s. The ring fades in over 0.9 s. `logo.js` removes `.is-assembling` after 1.5 s, so the mark showing again later (a taller window) does not replay it. |
| After that | The cyan pixel pulses (opacity 1 to 0.35 and back) every 3.2 s at idle. |
| CPU load | Each snapshot sets the pulse's `playbackRate` from `cpu.usage`: 1 at 0%, 3 at 100% (about one pulse a second), in 0.25 steps. `playbackRate` keeps the current phase, so a change never jumps; no layout or style writes per tick. |

Off means off: with Settings > Display > Animations off, with Windows' "Show animations" off,
or while the window is hidden (tray, minimised), `.is-pulsing` is removed and the global
reduce-motion rules drop the assembly, so the finished mark shows with no animation at all.
Transform and opacity only.

## Accessibility

A plain `<button>` labelled "LegacyTech" (tooltip "About Orbit"), in the Tab order between
Settings and the Live / System card. Up / Down in the sidebar still move between nav items
only. The mark itself is `aria-hidden`.

## Checks

`tests/ui-smoke.mjs`: in every theme "logo" checks the label, seven blocks, the kit colours
for that theme and that it sits clear of the nav and the card. "logo" then checks the pulse
runs, playbackRate 3 / 1 / 2 at 100 / 0 / 50% CPU, no animation while `document.hidden`, with
the Animations switch off, under `prefers-reduced-motion` and on a fresh start with the switch
off; not drawn in a 620px window (card 12px under the nav, as before); a click opens Settings
with About in view; Tab moves on to the Live / System card.
