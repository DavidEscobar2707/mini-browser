// window.__miniQA.probe(rootId)
//
// Geometry and text facts, computed in code, never asked of Jev: elements
// that overflow the viewport, clickable elements that overlap each other,
// undersized tap targets on narrow viewports, text clipped by
// overflow:hidden, large fixed/sticky overlays, accumulated layout shift,
// and junk text tokens. Scoped to the subtree registered by qa-sections.js
// when a root id is given, else the whole document. Reuses QA.__dom
// (qa-snapshot.js, injected before this file) for cssPath/visible/role/
// selector/textWalk so "clickable" and "path" match snapshot()'s idea of
// the same things.
(function () {
  const QA = (window.__miniQA = window.__miniQA || {});
  const dom = QA.__dom;

  // Armed once, at (document-start) load, so buffered:true still recovers
  // any shift that happened before this — the last of five init scripts —
  // actually ran.
  const cls = (QA.__cls = QA.__cls || { total: 0, supported: false, armed: false });
  if (!cls.armed) {
    cls.armed = true;
    try {
      if (
        typeof PerformanceObserver !== 'undefined' &&
        PerformanceObserver.supportedEntryTypes &&
        PerformanceObserver.supportedEntryTypes.includes('layout-shift')
      ) {
        cls.supported = true;
        new PerformanceObserver((list) => {
          for (const entry of list.getEntries()) if (!entry.hadRecentInput) cls.total += entry.value;
        }).observe({ type: 'layout-shift', buffered: true });
      }
    } catch (e) {
      // layout-shift unsupported; cls stays 0.
    }
  }

  const TAP_TARGETS = 'a[href],button,[role="button"],[role="link"],input[type="button"],input[type="submit"],input[type="reset"]';
  // \b requires a word-char neighbor; [ and ] are non-word, so a \b immediately
  // around the bracket literal can (almost) never match real text (no boundary
  // exists between two non-word characters). Keep \b for the three bare-word
  // tokens only; the bracket literal is already self-delimiting.
  const JUNK_TOKEN = /\b(?:undefined|NaN|Infinity)\b|\[object Object\]/;
  const JUNK_PHRASE = /lorem ipsum|TODO:|\{\{|\}\}/i;

  function resolveRoot(rootId) {
    if (!rootId) return document.body;
    const sections = QA.__sections;
    const el = sections && sections.byId && sections.byId.get(rootId);
    return el && el.isConnected ? el : null;
  }

  // querySelectorAll('*') excludes the root itself; include it so a scoped
  // root can flag its own geometry, but never the true document root.
  function candidateEls(root) {
    const self = root === document.body || root === document.documentElement ? [] : [root];
    return self.concat([...root.querySelectorAll('*')]);
  }

  // An element wider than the viewport is only a page-level defect when nothing
  // between it and the document handles the overflow: a scroll rail scrolls it,
  // and a clipping parent (`overflow-x: hidden`) crops it — both intended.
  function hasHorizontalScrollAncestor(el) {
    let node = el.parentElement;
    while (node && node !== document.documentElement) {
      const cs = getComputedStyle(node);
      if (cs.overflowX !== 'visible') return true;
      node = node.parentElement;
    }
    return false;
  }

  function findOverflow(root, isWholeDocument) {
    const items = [];
    for (const el of candidateEls(root)) {
      if (el === document.documentElement || el === document.body) continue;
      const r = el.getBoundingClientRect();
      if (r.width <= 0 || r.height <= 0) continue;
      const over_by = r.right - innerWidth;
      if (over_by > 4 && !hasHorizontalScrollAncestor(el)) items.push({ path: dom.cssPath(el), right: r.right, over_by });
    }
    if (isWholeDocument) {
      const de = document.documentElement;
      if (de.scrollWidth > de.clientWidth) {
        items.push({ path: 'document', right: de.scrollWidth, over_by: de.scrollWidth - de.clientWidth });
      }
    }
    return items;
  }

  function findOverlaps(root) {
    const els = [...root.querySelectorAll(dom.selector)].filter(dom.visible).slice(0, 300);
    const floating = (el) => {
      const cs = getComputedStyle(el);
      return cs.position === 'fixed' || cs.position === 'sticky';
    };
    const items = [];
    for (let i = 0; i < els.length; i++) {
      // A fixed/sticky bar floats over the content it scrolls past: that is
      // the layout working as designed, not two controls fighting for space.
      if (floating(els[i])) continue;
      const ra = els[i].getBoundingClientRect();
      const areaA = ra.width * ra.height;
      if (areaA <= 0) continue;
      for (let j = i + 1; j < els.length; j++) {
        if (floating(els[j])) continue;
        // A control inside another control is not a collision: a link in a card,
        // a day button in a cell — containment, not overlap.
        if (els[i].contains(els[j]) || els[j].contains(els[i])) continue;
        const rb = els[j].getBoundingClientRect();
        const areaB = rb.width * rb.height;
        if (areaB <= 0) continue;
        const iw = Math.min(ra.right, rb.right) - Math.max(ra.left, rb.left);
        const ih = Math.min(ra.bottom, rb.bottom) - Math.max(ra.top, rb.top);
        if (iw <= 0 || ih <= 0) continue;
        const area = iw * ih;
        const smaller = Math.min(areaA, areaB);
        if (area / smaller < 0.4) continue;
        // An overlap only matters when it *obscures* something: a favourite
        // button sitting on a card-wide link is deliberate stacking, and the
        // hit test at the smaller element's centre says which one it is.
        const small = areaA <= areaB ? els[i] : els[j];
        const sr = small.getBoundingClientRect();
        const hit = document.elementFromPoint(sr.x + sr.width / 2, sr.y + sr.height / 2);
        if (hit && (hit === small || small.contains(hit))) continue;
        items.push({ a: dom.cssPath(els[i]), b: dom.cssPath(els[j]), area });
      }
    }
    return items;
  }

  function findSmallTaps(root) {
    if (innerWidth > 480) return [];
    const items = [];
    for (const el of root.querySelectorAll(TAP_TARGETS)) {
      if (!dom.visible(el)) continue;
      const r = el.getBoundingClientRect();
      if (r.width <= 0 || r.height <= 0) continue;
      if (r.width < 44 || r.height < 44) items.push({ path: dom.cssPath(el), w: r.width, h: r.height, role: dom.role(el) || el.tagName.toLowerCase() });
    }
    return items;
  }

  function findClipped(root) {
    const items = [];
    for (const el of candidateEls(root)) {
      if (el === document.documentElement || el === document.body) continue;
      const cs = getComputedStyle(el);
      if (cs.overflowX !== 'hidden' && cs.overflow !== 'hidden') continue;
      // A box a few pixels wide that clips its text is intent, not a bug:
      // `sr-only` / `visually-hidden` spans measure 1px by design and every
      // screen reader label on the page would otherwise be reported.
      const r = el.getBoundingClientRect();
      if (r.width <= 4 || r.height <= 4) continue;
      if (el.clientWidth <= 4 || el.clientHeight <= 4) continue;
      // `text-overflow: ellipsis` is a signalled, intended truncation — the
      // user can see there is more. So is a carousel rail peeking a few pixels.
      if (cs.textOverflow === 'ellipsis') continue;
      const slack = Math.max(8, el.clientWidth * 0.02);
      if (el.scrollWidth > el.clientWidth + slack) {
        items.push({ path: dom.cssPath(el), scroll_w: el.scrollWidth, client_w: el.clientWidth });
      }
    }
    return items;
  }

  function findFixedOverlays(root) {
    const items = [];
    const viewportArea = innerWidth * innerHeight;
    for (const el of candidateEls(root)) {
      if (el === document.documentElement || el === document.body) continue;
      const cs = getComputedStyle(el);
      if (cs.position !== 'fixed' && cs.position !== 'sticky') continue;
      // A layer that cannot receive a pointer is decoration: it cannot cover
      // anything a user or the QA run was trying to reach.
      if (cs.pointerEvents === 'none') continue;
      if (cs.visibility === 'hidden') continue;
      if (!dom.visible(el)) continue;
      const r = el.getBoundingClientRect();
      const iw = Math.min(r.right, innerWidth) - Math.max(r.left, 0);
      const ih = Math.min(r.bottom, innerHeight) - Math.max(r.top, 0);
      if (iw <= 0 || ih <= 0) continue;
      const coverage = (iw * ih) / viewportArea;
      if (coverage >= 0.5) items.push({ path: dom.cssPath(el), coverage });
    }
    return items;
  }

  function findJunk(root) {
    const items = [];
    const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT);
    let node;
    while ((node = walker.nextNode())) {
      const parent = node.parentElement;
      if (!parent || parent.closest('script,style,noscript,template') || !dom.visible(parent)) continue;
      const text = node.textContent;
      const m = JUNK_TOKEN.exec(text) || JUNK_PHRASE.exec(text);
      if (!m) continue;
      const start = Math.max(0, m.index - 20);
      const snippet = text.slice(start, m.index + m[0].length + 20).trim();
      items.push({ path: dom.cssPath(parent), token: m[0], snippet });
    }
    return items;
  }

  QA.probe = function (rootId) {
    try {
      const root = resolveRoot(rootId);
      if (rootId && !root) return { ok: false, error: 'unknown root id: ' + rootId };
      const scope = root || document.body;
      const isWholeDocument = !rootId;
      const de = document.documentElement;
      return {
        ok: true,
        w: innerWidth,
        h: innerHeight,
        doc: { scroll_width: de.scrollWidth, client_width: de.clientWidth, scroll_height: de.scrollHeight },
        overflow: findOverflow(scope, isWholeDocument),
        overlaps: findOverlaps(scope),
        small_taps: findSmallTaps(scope),
        clipped: findClipped(scope),
        fixed_overlays: findFixedOverlays(scope),
        cls: cls.supported ? cls.total : 0,
        junk: findJunk(scope),
        text: dom.textWalk(scope, 6000),
      };
    } catch (e) {
      return { ok: false, error: String((e && e.message) || e) };
    }
  };
})();
