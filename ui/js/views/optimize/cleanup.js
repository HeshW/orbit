// Optimize > Cleanup (0.5, feature 7c): placeholder until that feature is built.
// Sub-tab API (see ../optimize.js): mount(paneEl, ctx), onShow(), onHide().
// Styles: ui/css/views/optimize/cleanup.css, classes prefixed cl-.

let root = null;

export function mount(paneEl) {
  root = paneEl;
  root.innerHTML = `
    <div class="panel op-soon">
      <svg class="icon" aria-hidden="true"><use href="#i-disk"/></svg>
      <div>
        <p class="op-soon-title">Disk cleanup and what fills your disk</p>
        <p class="op-soon-body">Shows how much space temp files, the Windows Update cache, crash dumps, browser caches and the Recycle Bin take, and clears only what you tick. A biggest-folders view helps find what fills each drive.</p>
        <p class="micro">Coming in Orbit 0.5</p>
      </div>
    </div>`;
}

export function onShow() {}

export function onHide() {}
