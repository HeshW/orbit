// 03 / History: per-app CPU time, memory, I/O and network totals over past days.
// v0.4 scaffold: a placeholder until the history feature builds it (docs/V04-CONTRACT.md).

let root = null;
let visible = false;

const icon = (id) => `<svg class="icon" aria-hidden="true"><use href="#${id}"/></svg>`;

export function mount(rootEl) {
  root = rootEl;
  root.innerHTML = `
    <header class="view-head">
      <div>
        <p class="micro">03 / History</p>
        <h1 class="view-title">What ran. <em>What it cost.</em></h1>
      </div>
    </header>
    <div class="panel history-placeholder">
      <div class="empty">
        ${icon('i-history')}
        <strong>App history is coming in Orbit 0.4.</strong>
        <p>Orbit will keep its own daily totals for each app, on this PC only.</p>
      </div>
    </div>`;
}

export function onShow() {
  visible = true;
}

export function onHide() {
  visible = false;
}

export function isVisible() {
  return visible && !!root;
}
