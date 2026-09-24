// Portable forensic recorder. Same capture set as Mini's built-in
// DEBUG_RECORDER_SCRIPT (src/main.rs): console/errors, per-event DOM
// mutations with rects, rAF paint timeline, fetch/XHR/WS/SSE, React
// mount/unmount, and __miniDebug.run(label, incident) run rotation.
// Transport differs: posts via the __miniPost CDP binding exposed by
// tools/record.js (falls back to window.ipc inside Mini itself).
// Inject with Page.addScriptToEvaluateOnNewDocument so it runs before
// page scripts — required for the React DevTools hook to be seen.
(() => {
  if (window.__miniDebug) return;
  window.__miniDebug = true;
  const RECT_CAP = 40;
  const PREVIEW_CAP = 200;
  const FIBER_NODE_CAP = 2000;
  const T = () => performance.timeOrigin + performance.now();
  // Batched, rate-capped transport. One IPC message per event froze pages
  // that mount thousands of React components (the host round-trip per line
  // backs up the renderer). Lines queue here and flush every 100 ms as ONE
  // `debug-batch` message; past LINE_BUDGET lines per second the rest are
  // counted and reported as a single `dropped` line instead of sent.
  const LINE_BUDGET = 1500;
  const queue = [];
  let windowStart = 0, sentInWindow = 0, dropped = 0, flushTimer = 0;
  const flush = () => {
    flushTimer = 0;
    if (dropped) {
      queue.push(JSON.stringify({ cmd: 'debug', kind: 'console', level: 'warn', t: T(), url: location.href,
        args: ['[mini recorder] dropped ' + dropped + ' events over the ' + LINE_BUDGET + '/s budget'] }));
      dropped = 0;
    }
    if (!queue.length) return;
    const lines = queue.splice(0);
    try {
      if (typeof window.__miniPost === 'function') for (const l of lines) window.__miniPost(l);
      else if (window.ipc) window.ipc.postMessage(JSON.stringify({ cmd: 'debug-batch', lines }));
    } catch (_) {}
  };
  const post = (obj) => {
    try {
      const now = performance.now();
      if (now - windowStart > 1000) { windowStart = now; sentInWindow = 0; }
      if (++sentInWindow > LINE_BUDGET) { dropped++; return; }
      if (obj.url === undefined) obj.url = location.href;
      queue.push(JSON.stringify(obj));
      if (!flushTimer) flushTimer = setTimeout(flush, 100);
    } catch (_) {}
  };
  addEventListener('pagehide', flush);
  const s = (v) => {
    try {
      if (typeof v === 'string') return v;
      return JSON.stringify(v) ?? String(v);
    } catch (_) { return String(v); }
  };
  const path = (n) => {
    const parts = [];
    let el = n instanceof Element ? n : n && n.parentElement;
    for (let i = 0; el && i < 5; i++, el = el.parentElement) {
      parts.unshift(el.id ? el.tagName.toLowerCase() + '#' + el.id
        : el.tagName.toLowerCase()
          + (el.classList.length ? '.' + [...el.classList].slice(0, 3).join('.') : ''));
      if (el.id) break;
    }
    return parts.join('>') || String(n && n.nodeName || '?');
  };
  let rectBudget = 0;
  const rectOf = (el) => {
    if (rectBudget >= RECT_CAP) return null;
    rectBudget++;
    try {
      if (!(el instanceof Element)) return null;
      const r = el.getBoundingClientRect();
      return { x: Math.round(r.x), y: Math.round(r.y), w: Math.round(r.width), h: Math.round(r.height) };
    } catch (_) { return null; }
  };

  for (const level of ['log', 'info', 'warn', 'error', 'debug']) {
    const orig = console[level].bind(console);
    console[level] = (...args) => {
      post({ cmd: 'debug', kind: 'console', t: T(), url: location.href,
             level, args: args.map(s).slice(0, 20) });
      orig(...args);
    };
  }
  window.addEventListener('error', (e) => {
    post({ cmd: 'debug', kind: 'console', t: T(), url: location.href,
           level: 'uncaught', args: [String(e.message),
           (e.filename || '') + ':' + (e.lineno || 0) + ':' + (e.colno || 0)] });
  });
  window.addEventListener('unhandledrejection', (e) => {
    post({ cmd: 'debug', kind: 'console', t: T(), url: location.href,
           level: 'unhandledrejection', args: [s(e.reason)] });
  });

  const buf = [];
  const frameTs = [];
  const flush = () => {
    rectBudget = 0;
    if (buf.length) {
      post({ cmd: 'debug', kind: 'dom', t: T(), url: location.href,
             events: buf.splice(0, 500) });
    }
    if (frameTs.length) {
      post({ cmd: 'debug', kind: 'frame', t: T(), frames: frameTs.splice(0) });
    }
  };
  const start = () => {
    new MutationObserver((muts) => {
      for (const m of muts) {
        if (buf.length >= 2000) break;
        const ts = T();
        if (m.type === 'attributes') {
          const ev = { k: 'attr', p: path(m.target), a: m.attributeName,
            from: m.oldValue, to: m.target.getAttribute(m.attributeName), ts };
          if (['style', 'class', 'width', 'height', 'hidden', 'open'].indexOf(m.attributeName) >= 0) {
            ev.r = rectOf(m.target);
          }
          buf.push(ev);
        } else if (m.type === 'characterData') {
          buf.push({ k: 'text', p: path(m.target), ts });
        } else {
          buf.push({ k: 'children', p: path(m.target),
            add: [...m.addedNodes].map(path).slice(0, 10),
            del: [...m.removedNodes].map(path).slice(0, 10),
            ts, r: rectOf(m.target) });
        }
      }
    }).observe(document.documentElement, {
      subtree: true, childList: true, characterData: true,
      attributes: true, attributeOldValue: true,
    });
    setInterval(flush, 250);
  };
  document.documentElement ? start() : document.addEventListener('DOMContentLoaded', start);
  const onFrame = () => { frameTs.push(T()); requestAnimationFrame(onFrame); };
  requestAnimationFrame(onFrame);

  let netId = 0;
  try {
    const origFetch = window.fetch.bind(window);
    window.fetch = function(input, init) {
      const id = ++netId;
      let url = '';
      let method = 'GET';
      try {
        if (typeof input === 'string') url = input;
        else if (input && typeof input.url === 'string') url = input.url;
        else url = String(input);
        method = (init && init.method) || (input && input.method) || 'GET';
      } catch (_) {}
      const t0 = T();
      post({ cmd: 'debug', kind: 'net', t: t0, url, api: 'fetch', id, phase: 'send', method });
      try {
        return origFetch(input, init).then((res) => {
          post({ cmd: 'debug', kind: 'net', t: T(), url, api: 'fetch', id, phase: 'recv',
                 status: res.status, ms: T() - t0 });
          return res;
        }, (err) => {
          post({ cmd: 'debug', kind: 'net', t: T(), url, api: 'fetch', id, phase: 'error',
                 error: String(err) });
          throw err;
        });
      } catch (err) {
        post({ cmd: 'debug', kind: 'net', t: T(), url, api: 'fetch', id, phase: 'error',
               error: String(err) });
        throw err;
      }
    };
  } catch (_) {}
  try {
    const origOpen = XMLHttpRequest.prototype.open;
    const origSend = XMLHttpRequest.prototype.send;
    XMLHttpRequest.prototype.open = function(method, url, ...rest) {
      this.__miniNet = { method: String(method || 'GET'), url: String(url || ''), id: ++netId };
      return origOpen.call(this, method, url, ...rest);
    };
    XMLHttpRequest.prototype.send = function(...args) {
      const meta = this.__miniNet || { method: 'GET', url: '', id: ++netId };
      post({ cmd: 'debug', kind: 'net', t: T(), url: meta.url, api: 'xhr', id: meta.id,
             phase: 'send', method: meta.method });
      this.addEventListener('loadend', () => {
        post({ cmd: 'debug', kind: 'net', t: T(), url: meta.url, api: 'xhr', id: meta.id,
               phase: 'recv', status: this.status });
      });
      return origSend.apply(this, args);
    };
  } catch (_) {}
  try {
    if (typeof WebSocket !== 'undefined') {
      const OrigWS = WebSocket;
      window.WebSocket = class extends OrigWS {
        constructor(url, protocols) {
          super(url, protocols);
          const id = ++netId;
          const u = String(url);
          post({ cmd: 'debug', kind: 'net', t: T(), url: u, api: 'ws', id, phase: 'open' });
          this.addEventListener('message', (e) => {
            const data = e.data;
            const bytes = typeof data === 'string' ? data.length
              : (data && data.byteLength) || 0;
            const ev = { cmd: 'debug', kind: 'net', t: T(), url: u, api: 'ws', id,
                         phase: 'message', bytes };
            if (typeof data === 'string') ev.preview = data.slice(0, PREVIEW_CAP);
            post(ev);
          });
          this.addEventListener('close', () => {
            post({ cmd: 'debug', kind: 'net', t: T(), url: u, api: 'ws', id, phase: 'close' });
          });
          this.addEventListener('error', () => {
            post({ cmd: 'debug', kind: 'net', t: T(), url: u, api: 'ws', id, phase: 'error' });
          });
        }
      };
    }
  } catch (_) {}
  try {
    if (typeof EventSource !== 'undefined') {
      const OrigES = EventSource;
      window.EventSource = class extends OrigES {
        constructor(url, config) {
          super(url, config);
          const id = ++netId;
          const u = String(url);
          post({ cmd: 'debug', kind: 'net', t: T(), url: u, api: 'sse', id, phase: 'open' });
          this.addEventListener('message', (e) => {
            const data = e.data;
            const bytes = typeof data === 'string' ? data.length : 0;
            post({ cmd: 'debug', kind: 'net', t: T(), url: u, api: 'sse', id, phase: 'message',
                   event: e.type, bytes,
                   preview: typeof data === 'string' ? data.slice(0, PREVIEW_CAP) : undefined });
          });
          this.addEventListener('error', () => {
            post({ cmd: 'debug', kind: 'net', t: T(), url: u, api: 'sse', id, phase: 'error' });
          });
        }
      };
    }
  } catch (_) {}

  try {
    if (!window.__REACT_DEVTOOLS_GLOBAL_HOOK__) {
      const hostPathOf = (fiber) => {
        try {
          let f = fiber;
          for (let i = 0; f && i < 50; i++, f = f.return) {
            if (f.stateNode instanceof Element) return path(f.stateNode);
          }
        } catch (_) {}
        return '';
      };
      const isComp = (type) => typeof type === 'function';
      const compName = (type) => (type && (type.displayName || type.name)) || 'Anon';
      window.__REACT_DEVTOOLS_GLOBAL_HOOK__ = {
        isDisabled: false,
        supportsFiber: true,
        renderers: new Map(),
        inject(r) {
          const id = this.renderers.size + 1;
          this.renderers.set(id, r);
          return id;
        },
        onScheduleFiberRoot() {},
        onCommitFiberRoot(id, root) {
          try {
            let n = 0;
            const walk = (fiber) => {
              if (!fiber || n++ >= FIBER_NODE_CAP) return;
              if (fiber.alternate === null && isComp(fiber.type)) {
                post({ cmd: 'debug', kind: 'react', t: T(), op: 'mount',
                       name: compName(fiber.type), path: hostPathOf(fiber) });
              }
              walk(fiber.child);
              walk(fiber.sibling);
            };
            walk(root && root.current);
          } catch (_) {}
        },
        onCommitFiberUnmount(id, fiber) {
          try {
            if (fiber && isComp(fiber.type)) {
              post({ cmd: 'debug', kind: 'react', t: T(), op: 'unmount',
                     name: compName(fiber.type), path: hostPathOf(fiber) });
            }
          } catch (_) {}
        },
        on() {},
        off() {},
        sub() { return () => {}; },
      };
    }
  } catch (_) {}

  // User/agent attention marks. Ctrl+Shift+Click any element (capture phase,
  // swallowed before the app sees it) or call __miniDebug.mark(sel, note).
  // Marks are a first-class stream: the analyzer flags findings whose DOM
  // path falls inside a marked subtree.
  const markOf = (el, note, via) => {
    if (!(el instanceof Element)) return null;
    const r = el.getBoundingClientRect();
    return {
      cmd: 'debug', kind: 'mark', t: T(), via, note: note || '',
      path: path(el),
      sel: el.id ? '#' + el.id
        : el.getAttribute('data-testid') ? '[data-testid="' + el.getAttribute('data-testid') + '"]'
        : path(el),
      r: { x: Math.round(r.x), y: Math.round(r.y), w: Math.round(r.width), h: Math.round(r.height) },
      text: (el.textContent || '').trim().slice(0, 120),
      html: el.outerHTML.slice(0, 600),
    };
  };
  const flashMark = (el) => {
    try {
      const prev = el.style.outline;
      el.style.outline = '3px solid #ff2d78';
      setTimeout(() => { el.style.outline = prev; }, 900);
    } catch (_) {}
  };
  try {
    window.addEventListener('click', (e) => {
      if (!(e.ctrlKey && e.shiftKey)) return;
      const m = markOf(e.target, '', 'user-click');
      if (!m) return;
      e.preventDefault();
      e.stopPropagation();
      flashMark(e.target);
      post(m);
    }, true);
  } catch (_) {}

  window.__miniDebug = {
    run: (label, incident) => post({ cmd: 'debug', kind: 'run', label, incident }),
    mark: (target, note) => {
      try {
        const el = typeof target === 'string' ? document.querySelector(target) : target;
        const m = markOf(el, note, 'api');
        if (m) { flashMark(el); post(m); }
        return !!m;
      } catch (_) { return false; }
    },
  };
})();
