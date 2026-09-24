// window.__miniQA.sections()
//
// Landmarks (main/section/nav/aside/header/footer/[role]) plus h1-h3, in
// document order, capped at 50. Each element's id is assigned once and kept
// for the page's lifetime in a page-side registry (QA.__sections) so
// snapshot(rootId) and probe(rootId) can resolve a root by id later.
(function () {
  const QA = (window.__miniQA = window.__miniQA || {});
  const registry = (QA.__sections = QA.__sections || { byId: new Map(), byEl: new WeakMap(), next: 1 });

  const LANDMARK_SELECTOR = 'main,section,nav,aside,header,footer,[role],h1,h2,h3';

  // Minimal, local copy: qa-sections.js is injected before qa-snapshot.js
  // (which owns the shared QA.__dom helpers), so it cannot depend on it.
  const visible = (el) => {
    if (el.closest('[aria-hidden="true"],[inert]')) return false;
    return typeof el.checkVisibility === 'function'
      ? el.checkVisibility({ checkOpacity: true, checkVisibilityCSS: true })
      : el.offsetParent !== null;
  };

  const textOf = (el, cap) => {
    const raw = el.innerText !== undefined ? el.innerText : el.textContent || '';
    return raw.trim().replace(/\s+/g, ' ').slice(0, cap || 140);
  };

  const headingFor = (el) => {
    if (/^H[1-3]$/.test(el.tagName)) return textOf(el, 140);
    const labelledby = el.getAttribute('aria-labelledby');
    if (labelledby) {
      const ref = document.getElementById(labelledby.split(/\s+/)[0]);
      if (ref) return textOf(ref, 140);
    }
    const aria = el.getAttribute('aria-label');
    if (aria) return aria.trim().slice(0, 140);
    const h = el.querySelector('h1,h2,h3');
    if (h && visible(h)) return textOf(h, 140);
    return '';
  };

  QA.sections = function () {
    try {
      const found = document.querySelectorAll(LANDMARK_SELECTOR);
      const out = [];
      for (const el of found) {
        if (out.length >= 50) break;
        if (!visible(el)) continue;
        let id = registry.byEl.get(el);
        if (!id) {
          id = 's' + registry.next++;
          registry.byEl.set(el, id);
          registry.byId.set(id, el);
        }
        out.push({ id, tag: el.tagName.toLowerCase(), heading: headingFor(el), snippet: textOf(el, 140) });
      }
      return { ok: true, sections: out };
    } catch (e) {
      return { ok: false, error: String((e && e.message) || e) };
    }
  };
})();
