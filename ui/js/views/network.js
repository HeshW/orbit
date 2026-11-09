// 07 / Network: open connections and listening ports, with the process behind each.
// v0.4 scaffold: a placeholder until the network feature builds it (docs/V04-CONTRACT.md).

let root = null;
let visible = false;

const icon = (id) => `<svg class="icon" aria-hidden="true"><use href="#${id}"/></svg>`;

export function mount(rootEl) {
  root = rootEl;
  root.innerHTML = `
    <header class="view-head">
      <div>
        <p class="micro">07 / Network</p>
        <h1 class="view-title">Every connection. <em>Every port.</em></h1>
      </div>
    </header>
    <div class="panel network-placeholder">
      <div class="empty">
        ${icon('i-plug')}
        <strong>The Network view is coming in Orbit 0.4.</strong>
        <p>It will show every open connection and listening port, and which app owns it.</p>
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
