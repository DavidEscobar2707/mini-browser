// window.__miniQA.arm() / actLog() / act(op, target) / hitTest(id)
//
// arm() resets the recording window and (once, idempotently) installs
// capturing listeners for beforeinput/input/change/click/submit, plus
// history/pushState navigation tracking and fetch/XHR write-attempt
// counting through qa-settle.js's QA.__net hook (never repatches
// fetch/XHR itself — one monkey-patch site only).
//
// act() performs the operations that don't need real synthetic input
// (select/scroll_down/scroll_up/wait/focus); click/fill happen through the
// Browser trait's real mouse/keyboard input, guided by hitTest(). Both
// address elements by the string `id` a prior snapshot() assigned to an
// action (e.g. "e5"), resolved through QA.__lastActions + the QA.__dom
// node-identity cache (qa-snapshot.js, injected before this file).
(function () {
  const QA = (window.__miniQA = window.__miniQA || {});
  const dom = QA.__dom;

  const MAX_EVENTS = 500;
  const state = (QA.__act = QA.__act || { installed: false, armed: false, events: [], submits: 0, posts: 0, navigations: 0 });

  const shortTarget = (el) => {
    if (!el || el.nodeType !== 1) return null;
    return dom ? dom.cssPath(el) : el.tagName ? el.tagName.toLowerCase() : null;
  };

  const record = (kind, target) => {
    if (!state.armed) return;
    state.events.push({ kind, target, ts: performance.now() });
    if (state.events.length > MAX_EVENTS) state.events.shift();
    if (kind === 'submit') state.submits++;
  };

  const isWrite = (method) => {
    const m = String(method || 'GET').toUpperCase();
    return m !== 'GET' && m !== 'HEAD' && m !== 'OPTIONS';
  };

  // Resolves a snapshot() action id to its live DOM node, or null if the id
  // is unknown or the node has since left the document.
  const resolveLiveTarget = (id) => {
    const action = QA.__lastActions && QA.__lastActions[id];
    if (!action || !dom) return null;
    const el = dom.nodeFor(action.node);
    return el && el.isConnected ? el : null;
  };

  if (!state.installed) {
    state.installed = true;

    for (const kind of ['beforeinput', 'input', 'change', 'click', 'submit']) {
      document.addEventListener(kind, (e) => record(kind, shortTarget(e.target)), true);
    }

    const net = QA.__net || (QA.__net = { listeners: [] });
    net.listeners.push((method) => {
      if (state.armed && isWrite(method)) state.posts++;
    });

    const bumpNav = () => {
      if (state.armed) state.navigations++;
    };
    window.addEventListener('beforeunload', bumpNav, true);
    window.addEventListener('popstate', bumpNav, true);
    window.addEventListener('hashchange', bumpNav, true);
    if (window.history && history.pushState) {
      const origPush = history.pushState;
      history.pushState = function () {
        bumpNav();
        return origPush.apply(this, arguments);
      };
    }
    if (window.history && history.replaceState) {
      const origReplace = history.replaceState;
      history.replaceState = function () {
        bumpNav();
        return origReplace.apply(this, arguments);
      };
    }
    if (window.HTMLFormElement) {
      // form.submit() does not dispatch a 'submit' event by spec; count it explicitly.
      const origSubmit = HTMLFormElement.prototype.submit;
      HTMLFormElement.prototype.submit = function () {
        record('submit', shortTarget(this));
        return origSubmit.apply(this, arguments);
      };
    }
  }

  QA.arm = function () {
    state.armed = true;
    state.events = [];
    state.submits = 0;
    state.posts = 0;
    state.navigations = 0;
    return { ok: true };
  };

  QA.actLog = function () {
    return { ok: true, events: state.events.slice(), submits: state.submits, posts: state.posts, navigations: state.navigations };
  };

  QA.act = function (op, target) {
    try {
      switch (op) {
        case 'scroll_down':
          window.scrollBy(0, 560);
          return { ok: true, effect: 'scrolled down 560px to y=' + Math.round(scrollY) };
        case 'scroll_up':
          window.scrollBy(0, -560);
          return { ok: true, effect: 'scrolled up 560px to y=' + Math.round(scrollY) };
        case 'wait':
          return { ok: true, effect: 'waited' };
        case 'focus': {
          const el = resolveLiveTarget(target);
          if (!el) return { ok: false, error: 'unknown target: ' + target };
          el.focus();
          return { ok: true, effect: 'focused ' + shortTarget(el) };
        }
        case 'select': {
          const action = QA.__lastActions && QA.__lastActions[target];
          if (!action || action.kind !== 'select') return { ok: false, error: 'unknown target: ' + target };
          const el = resolveLiveTarget(target);
          if (!el) return { ok: false, error: 'stale target: ' + target };
          el.value = action.value;
          el.dispatchEvent(new Event('input', { bubbles: true }));
          el.dispatchEvent(new Event('change', { bubbles: true }));
          return { ok: true, effect: 'selected ' + JSON.stringify(action.value) + ' in ' + shortTarget(el) };
        }
        default:
          return { ok: false, error: 'unknown op: ' + op };
      }
    } catch (e) {
      return { ok: false, error: String((e && e.message) || e) };
    }
  };

  // A target is reachable when *any* point inside it is. Sample the centre and
  // the quadrant midpoints: an overlay that covers the middle of a wide control
  // (a floating bar over the bottom half, say) leaves the rest clickable.
  const samplePoints = (r) => {
    const xs = [r.x + r.width / 2, r.x + r.width * 0.25, r.x + r.width * 0.75];
    const ys = [r.y + r.height / 2, r.y + r.height * 0.25, r.y + r.height * 0.75];
    const points = [];
    for (const y of ys) for (const x of xs) points.push([x, y]);
    return points.filter(([x, y]) => x >= 0 && y >= 0 && x < innerWidth && y < innerHeight);
  };

  // `elementFromPoint` skips pointer-events:none, but a decorative layer with
  // the pointer passed through can still be reported as topmost by geometry.
  const exposedAt = (el, x, y) => {
    const hit = document.elementFromPoint(x, y);
    if (!hit) return false;
    if (hit === el || el.contains(hit)) return true;
    const stack = document.elementsFromPoint(x, y);
    const target = stack.indexOf(el);
    if (target <= 0) return false;
    return stack.slice(0, target).every((node) => {
      if (node === document.documentElement || node === document.body) return true;
      const cs = getComputedStyle(node);
      return cs.pointerEvents === 'none' || cs.visibility === 'hidden';
    });
  };

  QA.hitTest = function (id) {
    const action = QA.__lastActions && QA.__lastActions[id];
    if (!action || !dom) return { ok: false, error: 'unknown id: ' + id };
    const el = dom.nodeFor(action.node);
    if (!el) return { ok: false, error: 'unknown id: ' + id };
    if (!el.isConnected) return { ok: true, x: 0, y: 0, topmost: false, reason: 'disconnected' };
    if (!dom.visible(el)) return { ok: true, x: 0, y: 0, topmost: false, reason: 'not-visible' };
    const r = el.getBoundingClientRect();
    for (const [x, y] of samplePoints(r)) {
      if (exposedAt(el, x, y)) return { ok: true, x, y, topmost: true, reason: null };
    }
    const x = r.x + r.width / 2;
    const y = r.y + r.height / 2;
    const hit = document.elementFromPoint(x, y);
    return { ok: true, x, y, topmost: false, reason: 'covered by ' + shortTarget(hit) };
  };
})();
