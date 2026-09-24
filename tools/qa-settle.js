// window.__miniQA.settleStart() / settleState()
//
// Arms a MutationObserver, fetch/XHR in-flight counters, and a perpetual
// rAF frame counter at document start (this is the first of five
// initialization scripts, injected before sections/snapshot/act/probe).
//
// settled = no DOM mutation for >=300ms, spanning >=2 animation frames,
// with zero fetch/XHR requests currently in flight.
//
// __net is the single monkey-patch site for fetch/XMLHttpRequest; qa-act.js
// subscribes to QA.__net.listeners instead of patching these globals again.
(function () {
  const QA = (window.__miniQA = window.__miniQA || {});

  if (!QA.__settle) {
    const state = (QA.__settle = {
      t0: performance.now(),
      windowMutations: 0,
      lastMutationTime: performance.now(),
      frameCount: 0,
      lastMutationFrame: 0,
      inflight: 0,
    });

    const onMutation = () => {
      state.windowMutations++;
      state.lastMutationTime = performance.now();
      state.lastMutationFrame = state.frameCount;
    };
    try {
      new MutationObserver(onMutation).observe(document, {
        childList: true,
        subtree: true,
        attributes: true,
        characterData: true,
      });
    } catch (e) {
      // No document to observe yet; nothing earlier than "document start" to hook.
    }

    const tick = () => {
      state.frameCount++;
      requestAnimationFrame(tick);
    };
    requestAnimationFrame(tick);

    const trackStart = () => {
      state.inflight++;
    };
    const trackEnd = () => {
      state.inflight = Math.max(0, state.inflight - 1);
    };

    const net = (QA.__net = QA.__net || { listeners: [] });
    net.notify = (method, url) => {
      for (const fn of net.listeners) {
        try {
          fn(method, url);
        } catch (e) {
          // One bad listener must not break settle tracking.
        }
      }
    };

    if (typeof window.fetch === 'function') {
      const origFetch = window.fetch;
      window.fetch = function (input, init) {
        const method = (init && init.method) || (input && input.method) || 'GET';
        const url = typeof input === 'string' ? input : (input && input.url) || '';
        net.notify(String(method).toUpperCase(), url);
        trackStart();
        return origFetch.call(this, input, init).finally(trackEnd);
      };
    }

    if (typeof window.XMLHttpRequest === 'function') {
      const proto = XMLHttpRequest.prototype;
      const origOpen = proto.open;
      const origSend = proto.send;
      proto.open = function (method, url) {
        this.__qaMethod = method;
        this.__qaUrl = url;
        return origOpen.apply(this, arguments);
      };
      proto.send = function () {
        net.notify(String(this.__qaMethod || 'GET').toUpperCase(), this.__qaUrl || '');
        trackStart();
        this.addEventListener('loadend', trackEnd, { once: true });
        return origSend.apply(this, arguments);
      };
    }
  }

  const s = QA.__settle;

  QA.settleStart = function () {
    s.t0 = performance.now();
    s.windowMutations = 0;
    return { ok: true, t0: s.t0 };
  };

  QA.settleState = function () {
    const now = performance.now();
    const ms = Math.max(0, Math.round(now - s.lastMutationTime));
    const framesSince = s.frameCount - s.lastMutationFrame;
    // requestAnimationFrame never fires while the document is hidden (tab
    // backgrounded / window minimized) — bypass the frame gate in that case
    // so a hidden page can still settle, instead of deadlocking settled:false
    // forever with nothing left to render.
    const frameGateOk = document.hidden || framesSince >= 2;
    const settled = ms >= 300 && frameGateOk && s.inflight === 0;
    return { ok: true, settled, ms, mutations: s.windowMutations, inflight: s.inflight };
  };
})();
