// A minimal DOM for testing `assets/app.js` in Node: just enough surface for
// the shim, nothing more. The shim is loaded into a `vm` context whose
// globals come from here, so every listener it registers is dispatchable and
// every fetch is spyable.
//
// No npm dependency on purpose: the repo is a Rust workspace, CI runners have
// Node, and the fidelity that matters (focus, events, fetch) is exactly the
// fidelity written here.

export class El {
  constructor(tag) {
    this.tagName = String(tag).toUpperCase();
    this.id = "";
    this.className = "";
    this.dataset = {};
    this.style = {};
    this.children = [];
    this.parentElement = null;
    this.disabled = false;
    this.value = "";
    this.attributes = {};
    this.textContent = "";
    this.offsetWidth = 100;
    this.offsetHeight = 100;
    this._rect = { left: 0, top: 0, width: 0, height: 0, right: 0, bottom: 0 };
    this._classes = new Set();
    this._listeners = null; // owned by the document, not the element
    this._html = "";
  }

  get classList() {
    const set = this._classes;
    return {
      add: (...cs) => cs.forEach((c) => set.add(c)),
      remove: (...cs) => cs.forEach((c) => set.delete(c)),
      contains: (c) => set.has(c),
    };
  }

  appendChild(child) {
    child.parentElement = this;
    this.children.push(child);
    return child;
  }

  // innerHTML is how frames land: the string is kept verbatim, and stub
  // elements are minted for every id it mentions, so the shim's
  // getElementById keeps working against the "new page".
  set innerHTML(html) {
    this._html = String(html);
    this._doc._reindex(this._html);
  }

  get innerHTML() {
    return this._html;
  }

  getBoundingClientRect() {
    return { ...this._rect, right: this._rect.left + this._rect.width,
             bottom: this._rect.top + this._rect.height };
  }

  setRect(left, top, width, height) {
    this._rect = { left, top, width, height };
  }

  // The tiny selector vocabulary the shim uses: "#id", ".class", "tag",
  // "[attr]", comma-separated unions.
  matches(selector) {
    return selector.split(",").some((one) => {
      const s = one.trim();
      if (s.startsWith("#")) return this.id === s.slice(1);
      if (s.startsWith(".")) return this._classes.has(s.slice(1));
      if (s.startsWith("[")) {
        const name = s.slice(1, -1);
        // data-foo-bar lives in dataset as fooBar.
        const key = name.startsWith("data-")
          ? name.slice(5).replace(/-([a-z])/g, (_, c) => c.toUpperCase())
          : name;
        return key in this.dataset || name in this.attributes;
      }
      return this.tagName === s.toUpperCase();
    });
  }

  closest(selector) {
    let el = this;
    while (el) {
      if (el.matches && el.matches(selector)) return el;
      el = el.parentElement;
    }
    return null;
  }

  querySelectorAll() {
    return [];
  }

  focus() {
    this._doc.activeElement = this;
    this._doc._fire("focusin", { target: this });
  }

  blur() {
    if (this._doc.activeElement === this) this._doc.activeElement = null;
    this._doc._fire("focusout", { target: this });
  }

  setSelectionRange(a, b) {
    this.selectionStart = a;
    this.selectionEnd = b;
  }

  click() {
    this._doc._fire("click", {
      target: this,
      preventDefault() {},
      stopPropagation() {},
    });
  }
}

export class FakeDocument {
  constructor() {
    this.title = "";
    this.activeElement = null;
    this.body = new El("body");
    this.body._doc = this;
    this._byId = new Map();
    this._listeners = new Map();
    this._mount(this.body);
  }

  _mount(el) {
    el._doc = this;
    if (el.id) this._byId.set(el.id, el);
  }

  createElement(tag) {
    const el = new El(tag);
    el._doc = this;
    return el;
  }

  getElementById(id) {
    return this._byId.get(id) || null;
  }

  addEventListener(type, fn) {
    if (!this._listeners.has(type)) this._listeners.set(type, []);
    this._listeners.get(type).push(fn);
  }

  _fire(type, event) {
    for (const fn of this._listeners.get(type) || []) fn(event);
  }

  // An element the tests hang under the tree, e.g. a button with data-ev.
  attach(tag, { id, classes, dataset, parent } = {}) {
    const el = this.createElement(tag);
    if (id) el.id = id;
    for (const c of classes || []) el._classes.add(c);
    Object.assign(el.dataset, dataset || {});
    (parent || this.body).appendChild(el);
    this._mount(el);
    return el;
  }

  // The frame swap: ids mentioned in the new markup get fresh stubs.
  // Simplified, not exact: ids absent from the new markup linger (a real
  // DOM would drop them); no test depends on their absence.
  _reindex(html) {
    for (const [, id] of html.matchAll(/id="([^"]+)"/g)) {
      if (!this._byId.has(id)) {
        const el = this.createElement(id.includes("input") ? "input" : "div");
        el.id = id;
        const value = html.match(
          new RegExp(`id="${id}"[^>]*value="([^"]*)"`));
        if (value) el.value = value[1];
        this._mount(el);
      }
    }
  }
}

// The global context the shim runs in. `responses` is a queue or a function
// of ({ url, body }) => { status, json } the test controls; every call is
// recorded in `calls`.
export function makeContext({ search = "", responses, dark = true } = {}) {
  const document = new FakeDocument();
  const app = document.createElement("div");
  app.id = "app";
  document.body.appendChild(app);
  document._mount(app);

  const calls = [];
  const listeners = new Map();
  const rafQueue = [];
  // The shim's timers (poll chain, debounces) are unref'd so they can fire
  // during a test's own waits but never keep the process alive at the end.
  const unrefd = (fn, ms) => {
    const timer = setTimeout(fn, ms);
    if (timer.unref) timer.unref();
    return timer;
  };
  const context = {
    document,
    location: { search },
    URLSearchParams,
    performance,
    setTimeout: unrefd,
    clearTimeout,
    console,
    fetch: async (url, opts) => {
      calls.push({ url, body: opts && opts.body });
      const answer = responses ? await responses({ url, opts }) : { status: 204 };
      return {
        status: answer.status,
        ok: answer.status >= 200 && answer.status < 300,
        json: async () => answer.json || {},
        text: async () => answer.text || "",
      };
    },
    requestAnimationFrame: (cb) => rafQueue.push(cb),
    __listeners: listeners,
  };
  context.window = {
    innerWidth: 1366,
    innerHeight: 768,
    matchMedia: (query) => ({
      matches: query.includes("dark") === dark,
      addEventListener() {},
    }),
    addEventListener: (type, fn) => {
      if (!listeners.has(type)) listeners.set(type, []);
      listeners.get(type).push(fn);
    },
  };
  return {
    context,
    document,
    calls,
    // One animation frame: drain the current rAF queue once.
    frame() {
      const queue = rafQueue.splice(0);
      for (const cb of queue) cb(performance.now());
    },
    fireWindow(type, event) {
      for (const fn of listeners.get(type) || []) fn(event);
    },
  };
}
