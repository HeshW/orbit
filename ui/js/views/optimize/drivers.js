// Optimize > Drivers (0.5, feature 7e): placeholder until that feature is built.
// Sub-tab API (see ../optimize.js): mount(paneEl, ctx), onShow(), onHide().
// Styles: ui/css/views/optimize/drivers.css, classes prefixed dr-.

let root = null;

export function mount(paneEl) {
  root = paneEl;
  root.innerHTML = `
    <div class="panel op-soon">
      <svg class="icon" aria-hidden="true"><use href="#i-cpu"/></svg>
      <div>
        <p class="op-soon-title">Driver check</p>
        <p class="op-soon-body">Lists every device driver with its version, date, maker and signature, and flags devices that report a problem. Updates come only through Windows Update or the maker's own site.</p>
        <p class="micro">Coming in Orbit 0.5</p>
      </div>
    </div>`;
}

export function onShow() {}

export function onHide() {}
