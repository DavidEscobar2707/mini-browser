// window.__miniQA.snapshot(rootId)
//
// Ported from jev_ultrafast/jev_ultrafast/snapshot.js (MIT): element
// selection, name()/role()/visible()/safe(), the per-node identity cache,
// and the 6000-char visible-text walk are kept as-is. Extended with a CSS
// `path` (evidence), input_type/name/form_method/form_action/href_host/href
// disabled per action, and root scoping through the registry
// qa-sections.js maintains (QA.__sections).
//
// Defines QA.__dom, the shared DOM-helper namespace qa-act.js and
// qa-probe.js reuse (they are injected after this file).
(function () {
  const QA = (window.__miniQA = window.__miniQA || {});

  if (!QA.__dom) {
    const cache = { ids: new WeakMap(), nodes: new Map(), next: 1 };

    const identity = (e) => {
      if (!cache.ids.has(e)) cache.ids.set(e, cache.next++);
      const id = cache.ids.get(e);
      cache.nodes.set(id, e);
      return id;
    };
    // Raw lookup — may return a disconnected element. Callers that need a
    // live node check `.isConnected` themselves (hitTest reports *why*).
    const nodeFor = (id) => cache.nodes.get(id) || null;

    const safe = (e) => !['password', 'file', 'hidden'].includes(e.type);
    const visible = (e) =>
      !e.closest('[aria-hidden="true"],[inert]') && e.checkVisibility({ checkOpacity: true, checkVisibilityCSS: true });

    const name = (e, seen = new Set()) => {
      if (!e || seen.has(e)) return '';
      seen.add(e);
      const referenced = (e.getAttribute('aria-labelledby') || '')
        .split(/\s+/)
        .map((id) => name(document.getElementById(id), seen))
        .filter(Boolean)
        .join(' ');
      return (
        referenced ||
        e.getAttribute('aria-label') ||
        [...(e.labels || [])].map((l) => name(l, seen)).filter(Boolean).join(' ') ||
        (['button', 'submit', 'reset'].includes(e.type) ? e.value : '') ||
        e.getAttribute('alt') ||
        (e.tagName === 'INPUT'
          ? ''
          : [...e.childNodes]
              .map((n) =>
                n.nodeType === 3 ? n.textContent : n.nodeType === 1 && n.getAttribute('aria-hidden') !== 'true' ? name(n, seen) : ''
              )
              .join(' ')
              .trim()) ||
        e.getAttribute('title') ||
        e.getAttribute('placeholder') ||
        ''
      );
    };

    const ROLES = [
      'button', 'link', 'checkbox', 'radio', 'switch', 'tab', 'menuitem', 'menuitemradio',
      'option', 'gridcell', 'combobox', 'textbox', 'searchbox', 'spinbutton',
    ];
    const SELECTOR =
      'a[href],button,input,textarea,select,summary,[contenteditable="true"],' + ROLES.map((r) => `[role="${r}"]`).join(',');

    const role = (e) => {
      const explicit = e.getAttribute('role');
      if (ROLES.includes(explicit)) return explicit;
      if (e.tagName === 'BUTTON' || e.tagName === 'SUMMARY') return 'button';
      if (e.tagName === 'A') return 'link';
      if (e.tagName === 'SELECT') return 'combobox';
      if (e.tagName === 'TEXTAREA' || e.isContentEditable) return 'textbox';
      if (e.tagName === 'INPUT') {
        if (['checkbox', 'radio'].includes(e.type)) return e.type;
        if (['button', 'submit', 'reset', 'image'].includes(e.type)) return 'button';
        if (e.type === 'search') return 'searchbox';
        if (e.type === 'number') return 'spinbutton';
        if (['text', 'email', 'url', 'tel'].includes(e.type)) return 'textbox';
      }
      return null;
    };

    // Short CSS path (<=5 segments, closest-to-element-first window) for evidence.
    const cssPath = (el, maxSegments = 5) => {
      const segments = [];
      let node = el;
      while (node && node.nodeType === 1 && segments.length < maxSegments) {
        let seg = node.tagName.toLowerCase();
        if (node.id) {
          segments.unshift(seg + '#' + CSS.escape(node.id));
          break;
        }
        if (node.classList && node.classList.length) seg += '.' + CSS.escape(node.classList[0]);
        const parent = node.parentElement;
        if (parent) {
          const sameTag = [...parent.children].filter((c) => c.tagName === node.tagName);
          if (sameTag.length > 1) seg += `:nth-of-type(${sameTag.indexOf(node) + 1})`;
        }
        segments.unshift(seg);
        node = parent;
      }
      return segments.join(' > ');
    };

    const textWalk = (root, cap = 6000) => {
      const words = [];
      let length = 0;
      const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT);
      const range = document.createRange();
      let node;
      while ((node = walker.nextNode()) && length < cap) {
        const value = node.textContent.trim();
        const parent = node.parentElement;
        if (!value || !parent || parent.closest('script,style,noscript,template') || !visible(parent)) continue;
        range.selectNodeContents(node);
        const r = range.getBoundingClientRect();
        if (r.width > 0 && r.height > 0 && r.bottom > 0 && r.top < innerHeight && r.right > 0 && r.left < innerWidth) {
          words.push(value);
          length += value.length;
        }
      }
      return words.join('\n').slice(0, cap);
    };

    // 1-based <form> ids, stable for the document's lifetime, so a judge can
    // tell the search submit from the sign-in submit.
    const forms = { ids: new WeakMap(), next: 1 };
    const formId = (form) => {
      if (!form) return null;
      if (!forms.ids.has(form)) forms.ids.set(form, forms.next++);
      return forms.ids.get(form);
    };

    QA.__dom = { identity, nodeFor, safe, visible, name, role, selector: SELECTOR, cssPath, textWalk, cache, formId };
  }

  const dom = QA.__dom;

  function reachable(e, r) {
    const pts = [[r.x + r.width / 2, r.y + r.height / 2], [r.x + r.width * 0.25, r.y + r.height / 2], [r.x + r.width * 0.75, r.y + r.height / 2]];
    for (const [x, y] of pts) {
      if (x < 0 || y < 0 || x >= innerWidth || y >= innerHeight) continue;
      const hit = document.elementFromPoint(x, y);
      if (hit && (hit === e || e.contains(hit) || hit.contains(e) || (e.labels && [...e.labels].some((l) => l.contains(hit))))) return true;
    }
    return false;
  }

  function buildActions(root) {
    const actions = [];
    // `mini --move` sets __allowSecretFields so the operator can sign in to a
    // local app. A password field is then offered as a textbox, but its value
    // is never read. `--qa` never sets the flag.
    const secretOk = QA.__allowSecretFields === true;
    for (const e of root.querySelectorAll(dom.selector)) {
      const secret = secretOk && e.tagName === 'INPUT' && e.type === 'password';
      if ((!dom.safe(e) && !secret) || !dom.visible(e) || e.matches(':disabled') || e.closest('[aria-disabled="true"]')) continue;
      const r = e.getBoundingClientRect();
      const x = r.x + r.width / 2;
      const y = r.y + r.height / 2;
      const rname = secret ? 'textbox' : dom.role(e);
      // `mini --move`: every option of an open listbox is choosable, including
      // ones clipped by the list's own scroll area — the executor scrolls the
      // chosen option into view before clicking it.
      const listOption = QA.__onlyReachable === true && rname === 'option';
      const offscreen = x < 0 || y < 0 || x >= innerWidth || y >= innerHeight;
      if (!rname || r.width <= 0 || r.height <= 0 || (offscreen && !listOption)) continue;
      if (rname === 'gridcell' && e.querySelector('button,[role="button"]')) continue;
      // `mini --move` only: skip controls another layer covers (an open modal,
      // a cookie banner). Offering them makes the judge pick unclickable
      // targets forever. `--qa` keeps reporting covered controls as findings.
      if (QA.__onlyReachable === true && !listOption && !reachable(e, r)) continue;

      const form = e.closest('form');
      let href_host = null;
      let href = null;
      if (e.tagName === 'A' && e.href) {
        // Keep the raw attribute: `/checkout?step=2` is what the blocklist
        // matches on, and `new URL()` would only give us the absolute form.
        href = e.getAttribute('href');
        try {
          href_host = new URL(e.href, location.href).host || null;
        } catch (err) {
          href_host = null;
        }
      }
      const base = {
        node: dom.identity(e),
        role: rname,
        label: dom.name(e) || rname,
        rect: { x: r.x, y: r.y, w: r.width, h: r.height },
        path: dom.cssPath(e),
        input_type: e.tagName === 'INPUT' ? e.type : null,
        name: e.getAttribute('name') || null,
        form_method: form ? form.method.toLowerCase() : null,
        form_action: form ? form.action : null,
        form_id: dom.formId ? dom.formId(form) : null,
        href_host,
        href,
        disabled: e.matches(':disabled') || e.getAttribute('aria-disabled') === 'true',
      };
      for (const key of ['checked', 'selected', 'expanded']) {
        const value = e.getAttribute('aria-' + key);
        if (value !== null) base[key] = value;
      }
      if (['checkbox', 'radio'].includes(e.type)) base.checked = String(e.checked);

      if (e.tagName === 'SELECT') {
        for (const o of e.options) {
          const optgroup = o.closest('optgroup');
          if (o.selected || o.disabled || (optgroup && optgroup.disabled)) continue;
          actions.push({
            ...base,
            kind: 'select',
            value: o.value,
            current_value: [...e.selectedOptions].map((sel) => sel.label).join(', '),
            label: base.label + ' \u2192 ' + o.label,
          });
        }
      } else {
        const editable =
          !e.readOnly &&
          e.getAttribute('aria-readonly') !== 'true' &&
          (['textbox', 'searchbox', 'spinbutton'].includes(rname) ||
            (rname === 'combobox' && ['INPUT', 'TEXTAREA'].includes(e.tagName)));
        const value = secret ? (e.value ? '(filled)' : '') : 'value' in e ? String(e.value) : e.isContentEditable || rname === 'combobox' ? e.innerText.trim() : '';
        actions.push({ ...base, kind: editable ? 'fill' : 'click', value });
        if (editable) actions.push({ ...base, kind: 'click', value, label: 'Open ' + base.label });
      }
    }
    // `mini --move` only: div/span controls with a JS click handler and no role (Cinemark's showtime
    // buttons are `<div onclick>`). Without them Jev never sees "18:00" and clicks a nearby link instead.
    if (QA.__onlyReachable === true) {
      for (const e of root.querySelectorAll('div,span,li,td,img')) {
        if (typeof e.onclick !== 'function' && getComputedStyle(e).cursor !== 'pointer') continue;
        if (e.closest(dom.selector) || e.querySelector(dom.selector)) continue;
        const parent = e.parentElement;
        if (parent && (typeof parent.onclick === 'function') && !e.querySelector('*')) continue; // keep the outer handler
        const text = (e.getAttribute('aria-label') || e.innerText || e.getAttribute('alt') || '').trim().replace(/\s+/g, ' ');
        if (!text || text.length > 60 || !dom.visible(e)) continue;
        const r = e.getBoundingClientRect();
        const x = r.x + r.width / 2;
        const y = r.y + r.height / 2;
        if (r.width <= 0 || r.height <= 0 || x < 0 || y < 0 || x >= innerWidth || y >= innerHeight) continue;
        if (!reachable(e, r)) continue;
        // Short labels like "18:00" are ambiguous across repeated blocks: suffix the nearest
        // ancestor heading (the movie title on Cinemark) so the judge can pick the right one.
        let ctx = '';
        if (text.length <= 12) {
          // Prefer the nearest h1-h3 (a sub-heading like "HORARIOS DISPONIBLES" is an h4 and says nothing).
          let weak = '';
          for (let a = e.parentElement, depth = 0; a && a !== document.body && !ctx && depth < 8; a = a.parentElement, depth++) {
            const h = a.querySelector('h1,h2,h3');
            if (h && !h.contains(e)) ctx = (h.innerText || '').trim().replace(/\s+/g, ' ').slice(0, 50);
            else if (!weak) {
              const w = a.querySelector('h4,h5,h6,[class*="title" i]');
              if (w && !w.contains(e)) weak = (w.innerText || '').trim().replace(/\s+/g, ' ').slice(0, 50);
            }
          }
          if (!ctx) ctx = weak;
        }
        actions.push({
          node: dom.identity(e), role: 'button', label: ctx ? text + ' \u2014 ' + ctx : text, rect: { x: r.x, y: r.y, w: r.width, h: r.height },
          path: dom.cssPath(e), input_type: null, name: null, form_method: null, form_action: null,
          form_id: null, href_host: null, href: null, disabled: false, kind: 'click', value: '',
        });
      }
    }
    return actions;
  }

  function gotoTargets(root) {
    const out = [];
    const seen = new Set();
    for (const h of root.querySelectorAll('h1,h2,h3,[role="heading"]')) {
      if (out.length >= 60) break;
      const text = (h.innerText || '').trim().replace(/\s+/g, ' ');
      if (!text || text.length > 80 || seen.has(text.toLowerCase()) || !dom.visible(h)) continue;
      const r = h.getBoundingClientRect();
      if (r.width <= 0 || r.height <= 0) continue;
      const where = r.bottom <= 0 ? 'above' : r.top >= innerHeight ? 'below' : null;
      if (!where) continue; // on screen already: nothing to jump to
      seen.add(text.toLowerCase());
      out.push({
        node: dom.identity(h), role: 'heading', label: text + ' (' + where + ')', rect: { x: r.x, y: r.y, w: r.width, h: r.height },
        path: dom.cssPath(h), input_type: null, name: null, form_method: null, form_action: null,
        form_id: null, href_host: null, href: null, disabled: false, kind: 'goto', value: '',
      });
    }
    return out;
  }

  // Same fingerprint idea as the reference's cache.pageKey(): current values
  // of every safe form control in scope, keyed by stable node identity.
  function pageKeyInputs(root) {
    const out = [];
    for (const e of root.querySelectorAll('input,textarea,select')) {
      if (QA.__allowSecretFields === true && e.type === 'password') {
        out.push([dom.identity(e), e.value ? '(filled)' : '', e.disabled, e.readOnly]); // never the secret
        continue;
      }
      if (!dom.safe(e)) continue;
      out.push([dom.identity(e), e.value, e.checked, e.selectedIndex, e.disabled, e.readOnly]);
    }
    return out;
  }

  QA.snapshot = function (rootId) {
    try {
      if (!document.body) return { ok: false, error: 'document not ready' };
      let root = document.body;
      if (rootId) {
        const sections = QA.__sections;
        const found = sections && sections.byId && sections.byId.get(rootId);
        if (!found || !found.isConnected) return { ok: false, error: 'unknown root id: ' + rootId };
        root = found;
      }
      for (const [id, el] of dom.cache.nodes) if (!el.isConnected) dom.cache.nodes.delete(id);

      let actions = buildActions(root);
      const text = dom.textWalk(root, 6000);
      const height = document.documentElement.scrollHeight;
      const omitted_actions = Math.max(0, actions.length - 250);
      actions = actions.slice(0, 250);
      // `mini --move` only: off-screen section headings as GO_TO targets (the agent's Ctrl+F).
      // Long pages (Cinemark lists ~15 movies over ~8,600 px) were otherwise searched by blind
      // SCROLL_DOWN, and the judge wandered off to other controls before reaching the block.
      if (QA.__onlyReachable === true) actions.push(...gotoTargets(root));
      actions.forEach((a, i) => (a.id = 'e' + (i + 1)));
      if (scrollY + innerHeight < height - 2) actions.push({ id: 'scroll_down', kind: 'scroll', label: 'Scroll down', delta: 560 });
      if (scrollY > 0) actions.push({ id: 'scroll_up', kind: 'scroll', label: 'Scroll up', delta: -560 });
      actions.push({ id: 'wait', kind: 'wait', label: 'Wait for the page to update' });

      QA.__lastActions = {};
      for (const a of actions) QA.__lastActions[a.id] = a;

      const semantics = actions.map(({ rect, ...rest }) => rest);
      const inputs = pageKeyInputs(root);
      const page_key = [location.href, scrollX, scrollY, innerWidth, innerHeight, inputs];
      const marker = [location.href, scrollX, scrollY, innerWidth, innerHeight, document.title, text, semantics, inputs];

      return {
        ok: true,
        url: location.href,
        title: document.title,
        w: innerWidth,
        h: innerHeight,
        text,
        scroll: { y: scrollY, height },
        actions,
        omitted_actions,
        marker,
        page_key,
      };
    } catch (e) {
      return { ok: false, error: String((e && e.message) || e) };
    }
  };
})();
