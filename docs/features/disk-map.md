# Disk map (Biggest folders, Map)

Biggest folders in Optimize > Cleanup has a List / Map switch. Map shows the folder you are in as
boxes sized by space used, biggest first. It only reads what the scan already returned and never
deletes anything.

## Using it

- Click a folder box to look inside it. It uses the same measure-and-cache path as clicking a row.
- Backspace or right-click goes up one folder, like the Up button.
- Point at a box, or Tab / arrow keys to it, to see its full name and size under the map. "Open in
  File Explorer" (or the O key) opens that folder.
- "Other folders" (the small ones the scan did not list) and "Files in this folder" are boxes too,
  but they cannot be opened.
- Boxes too small for text still have a tooltip and an accessible name.
- The List / Map choice is kept until Orbit closes.

## How it works

- `ui/js/views/optimize/treemap.js`: `squarify(items, w, h)` is a pure squarified treemap layout;
  `mountMap(el, handlers)` draws one absolutely positioned button per box and redraws when the
  box resizes (ResizeObserver). `cleanup.js` wires it to the existing drill-down and open actions.
- Boxes are real buttons styled with `tokens.css` colours (`cl-tm-` in `cleanup.css`), so all four
  themes work with no redraw and text stays sharp on high-density screens.
- No Rust change and no new command: it uses `cleanup_folder_sizes` and `open_location`.

## Checked

`tests/ui-smoke.mjs` "disk map": measure `C:\`, switch to Map, boxes render with tooltips, a click
opens the folder, Backspace goes back up, the Map choice survives List and back.
