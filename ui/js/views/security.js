// 08 / Security: a scan of running processes: signatures, odd locations and other warning signs.
// v0.4 scaffold: a placeholder until the security feature builds it (docs/V04-CONTRACT.md).

let root = null;
let visible = false;

const icon = (id) => `<svg class="icon" aria-hidden="true"><use href="#${id}"/></svg>`;

export function mount(rootEl) {
  root = rootEl;
  root.innerHTML = `
    <header class="view-head">
      <div>
        <p class="micro">08 / Security</p>
        <h1 class="view-title">Check what runs. <em>Trust what stays.</em></h1>
      </div>
    </header>
    <div class="panel security-placeholder">
      <div class="empty">
        ${icon('i-radar')}
        <strong>The Security scan is coming in Orbit 0.4.</strong>
        <p>It will check each running program for a valid signature and other warning signs.</p>
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
