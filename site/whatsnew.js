// What's new: the five newest GitHub releases, fetched in the visitor's browser.
// Release notes are markdown from release.yml: "- " lines become list items, other lines
// paragraphs, **bold** stays bold. Everything else is shown as plain text.
(function () {
  var API = 'https://api.github.com/repos/HeshW/orbit/releases?per_page=5';
  var RELEASES = 'https://github.com/HeshW/orbit/releases';
  var list = document.getElementById('releases');
  if (!list) return;

  function esc(s) {
    return String(s).replace(/[&<>"']/g, function (c) {
      return { '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c];
    });
  }

  function inline(s) {
    return esc(s).replace(/\*\*(.+?)\*\*/g, '<strong>$1</strong>');
  }

  function notes(body) {
    var out = [];
    var items = [];
    function flush() {
      if (items.length) out.push('<ul>' + items.join('') + '</ul>');
      items = [];
    }
    String(body || '').split(/\r?\n/).forEach(function (raw) {
      var line = raw.trim();
      var m = /^[-*]\s+(.*)$/.exec(line);
      if (m) { items.push('<li>' + inline(m[1]) + '</li>'); return; }
      flush();
      if (line) out.push('<p>' + inline(line) + '</p>');
    });
    flush();
    return out.join('');
  }

  function date(iso) {
    var d = new Date(iso);
    if (isNaN(d)) return '';
    return d.toLocaleDateString('en-GB', { day: 'numeric', month: 'short', year: 'numeric' });
  }

  function fallback(msg) {
    list.innerHTML = '<p class="release-note">' + esc(msg) + ' See every version on <a href="' + RELEASES +
      '" target="_blank" rel="noopener noreferrer">GitHub Releases</a>.</p>';
    list.setAttribute('aria-busy', 'false');
  }

  function render(releases) {
    var html = releases.filter(function (r) { return r && !r.draft; }).map(function (r) {
      var version = r.tag_name || r.name || 'Release';
      var when = r.published_at || r.created_at;
      var url = /^https:\/\/github\.com\//.test(r.html_url || '') ? r.html_url : RELEASES;
      return '<article class="card release">' +
        '<header class="release-head">' +
          '<h3><a href="' + esc(url) + '" target="_blank" rel="noopener noreferrer">' + esc(version) + '</a></h3>' +
          (r.prerelease ? '<span class="micro">Pre-release</span>' : '') +
          (when ? '<time class="micro" datetime="' + esc(when) + '">' + esc(date(when)) + '</time>' : '') +
        '</header>' +
        '<div class="release-body">' + (notes(r.body) || '<p>No notes for this version.</p>') + '</div>' +
      '</article>';
    }).join('');
    if (!html) { fallback('No releases yet.'); return; }
    list.innerHTML = html;
    list.setAttribute('aria-busy', 'false');
  }

  if (typeof fetch !== 'function') { fallback("Release notes can't load in this browser."); return; }
  fetch(API, { headers: { Accept: 'application/vnd.github+json' } })
    .then(function (res) {
      if (!res.ok) throw new Error(String(res.status));
      return res.json();
    })
    .then(function (data) {
      if (!Array.isArray(data)) throw new Error('shape');
      render(data);
    })
    .catch(function () {
      fallback("Release notes couldn't load right now.");
    });
})();
