# Feature: Motion like the LegacyTech site (v0.6, 8d)

Buttons, cards and links react on hover, views rise in when you switch to them, and headline
numbers count up the first time a view shows. On by default; one switch turns all of it off.
UI only: no commands, events or new settings. Shapes are in `docs/CONTRACT.md` ("v0.6").

Files: `ui/js/motion.js` (new), `ui/css/tokens.css`, `ui/css/app.css`,
`ui/css/views/{performance,processes,lists}.css`, `ui/js/app.js` (`navigate`),
`ui/js/views/settings.js`, `ui/js/views/rig.js` (cards get `panel--hover`), `ui/js/palette.js`,
`tests/ui-smoke.mjs`.

## The switch

- Settings > Display > **Animations** (on by default). It is the old "Reduced motion" setting
  (`settings.reducedMotion`) shown the other way round, so saved settings and backups keep
  working. The palette offers "Turn animations off" / "Turn animations on".
- Windows' "Show animations" off (`prefers-reduced-motion: reduce`) stops the same things even
  with the switch on; the help line under the switch then says so.
- Off means zero cost: the global rules in `app.css` drop every transition and animation,
  `--lift` becomes 0px, the button sweep layer is not drawn, and `motion.js` starts nothing.
  Every element is already in its final place and fully visible without the animation.

## Motion language (legacytech.me design system)

Easing `cubic-bezier(0.2, 0.75, 0.18, 1)` (`--ease`); durations 0.25 s, 0.45 s, 0.9 s
(`--dur-1/2/3`). Transform and opacity only.

| Where | What |
|---|---|
| Buttons (`.btn`) | Lift 3px; a band of light sweeps across once (0.9 s); a trailing arrow (`i-arrow-right`, e.g. Health > "Go to Cleanup") turns -45deg. |
| Cards | My Rig cards, Performance tiles, theme cards and the sidebar's Live / System card lift 3px and glow. Table rows never move; process rows only fade their hover tint in. |
| Links | The underline draws in from the left (`.lv-textlink`, `.dr-link`); the arrow nudges right. |
| Views | On every sidebar switch the head, toolbar and main panels fade and rise 22px, 70 ms apart (capped at six steps, so the last block starts by 350 ms). |
| Numbers | First show of each view only: header stats (`.stat-value`), Performance readout and tile values, Health summary counts and the Cleanup total count up from 0 in 0.45 s. A number that arrives later (My Rig specs, Health report) counts when it lands, within 6 s. A live tick that writes a new value stops the count at once. Durations ("3h 20m"), times and dates are left alone. |
| Sidebar | The active marker slides in from the left. |
| Mini window | Hover transitions only (the "Open Orbit" button). |

## Cost

- Nothing runs continuously. The view entrance is a single finite CSS animation per block;
  its class is removed on `animationend` and it uses fill `backwards`, so no transform stays on
  a block afterwards (fixed and sticky children keep working).
- `motion.js` starts nothing while `document.hidden` (tray, minimised). A count-up is a short
  `requestAnimationFrame` loop that ends after 0.45 s or on the next live value.

## Checks

- `tests/ui-smoke.mjs` "motion": the switch is on by default; with it on a button and a card
  have 0.25 s transitions and a sweep, a view switch runs `orbit-enter 0.45s`, `countUp` starts
  at 0, and every head and panel ends at opacity 1; with the switch off, and again with
  `prefers-reduced-motion: reduce`, transitions are 0s, `--lift` is 0px, there is no sweep, no
  entrance and no count-up.
