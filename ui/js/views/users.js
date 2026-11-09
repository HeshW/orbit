// 05 / Users: signed-in Windows sessions with their CPU and memory, and session actions.
// v0.4 scaffold: a placeholder until the users feature builds it (docs/V04-CONTRACT.md).

let root = null;
let visible = false;

const icon = (id) => `<svg class="icon" aria-hidden="true"><use href="#${id}"/></svg>`;

export function mount(rootEl) {
  root = rootEl;
  root.innerHTML = `
    <header class="view-head">
      <div>
        <p class="micro">05 / Users</p>
        <h1 class="view-title">Who is on. <em>What they use.</em></h1>
      </div>
    </header>
    <div class="panel users-placeholder">
      <div class="empty">
        ${icon('i-users')}
        <strong>The Users view is coming in Orbit 0.4.</strong>
        <p>It will list everyone signed in to this PC, with the CPU and memory each session uses.</p>
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
