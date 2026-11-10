// Optimize > Boost (0.5, feature 7d): placeholder until that feature is built.
// Sub-tab API (see ../optimize.js): mount(paneEl, ctx), onShow(), onHide().
// Styles: ui/css/views/optimize/boost.css, classes prefixed bo-.

let root = null;

export function mount(paneEl) {
  root = paneEl;
  root.innerHTML = `
    <div class="panel op-soon">
      <svg class="icon" aria-hidden="true"><use href="#i-bolt"/></svg>
      <div>
        <p class="op-soon-title">Game and focus boost</p>
        <p class="op-soon-body">One click, or when a game you pick starts: lowers the priority of heavy background apps and switches to the High performance power plan. Everything goes back exactly as it was when you turn it off.</p>
        <p class="micro">Coming in Orbit 0.5</p>
      </div>
    </div>`;
}

export function onShow() {}

export function onHide() {}
