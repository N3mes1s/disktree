// The shim's contract, tested against a stub DOM: what it posts, what it
// paints, and that the tricky parts — the find field's focus, the wheel
// coalescing, the stale-answer ticket, the long-press — stay fixed.
//
// Run: node --test crates/disktree-web/tests-js/  (also via `make test`)

import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import vm from "node:vm";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

import { makeContext } from "./dom-stub.mjs";

const SOURCE = readFileSync(
  join(dirname(fileURLToPath(import.meta.url)), "../assets/app.js"),
  "utf8",
);

const wait = (ms) => new Promise((r) => setTimeout(r, ms));

// Load the shim fresh per test with its own DOM and fetch spy.
function boot({ search = "", responses } = {}) {
  const rig = makeContext({ search, responses });
  vm.createContext(rig.context);
  vm.runInContext(SOURCE, rig.context);
  return { ...rig, shim: rig.context };
}

const noDefault = () => ({ defaultPrevented: true, preventDefault() {} });

// A document carrying a mosaic the size of a real window.
function withMosaic(doc, w = 1000, h = 700) {
  const wrap = doc.attach("div", { id: "mosaic-wrap" });
  wrap.setRect(0, 0, w, h);
  const app = doc.getElementById("app");
  return wrap;
}

test("keys arrive with their GPUI names, guarded", async () => {
  const { document, calls } = boot();
  const fire = (key, extra = {}) =>
    document._fire("keydown", {
      key,
      target: document.body, // keydown always has a target in a browser
      ...noDefault(),
      ...extra,
    });

  fire("Enter");
  fire(" ");
  fire("ArrowLeft");
  fire("?");
  fire("x", { ctrlKey: true }); // chord: the browser's own, never sent
  fire("x");
  fire("F5"); // not the app's
  await wait(0);

  const keys = calls
    .filter((c) => c.body && c.body.includes('"key"'))
    .map((c) => JSON.parse(c.body).events[0].key);
  assert.deepEqual(keys, ["enter", "space", "left", "?", "x"]);
});

test("a control posts its data-ev verbatim; menu clicks gain an anchor", async () => {
  const { document, calls } = boot();
  const btn = document.attach("button", {
    dataset: { ev: '{"type":"key","key":"c"}' },
  });
  document._fire("click", {
    target: btn,
    ...noDefault(),
    stopPropagation() {},
  });
  // A disabled button is inert.
  const off = document.attach("button", {
    dataset: { ev: '{"type":"key","key":"x"}' },
  });
  off.disabled = true;
  document._fire("click", {
    target: off,
    ...noDefault(),
    stopPropagation() {},
  });
  await wait(0);
  const posted = calls.filter((c) => c.body).map((c) => JSON.parse(c.body));
  assert.equal(posted.at(-1).events[0].key, "c");
  assert.equal(posted.filter((p) => p.events[0].key === "x").length, 0);
});

test("a 204 changes nothing; html swaps the page; mosaic swaps the wrap only", async () => {
  const { document, context } = boot();
  const app = document.getElementById("app");
  app.innerHTML = 'before <div id="zoom-level"></div>';
  const shim = context;

  // 204: no change at all.
  await shim.send([]);
  assert.equal(app.innerHTML, 'before <div id="zoom-level"></div>');

  // A full frame swaps #app and the title.
  await shim.apply({
    html: '<div id="zoom-level"></div><div id="panel"></div>',
    title: "disktree · /tmp",
    busy: false,
    find_open: false,
    find: "",
  });
  assert.equal(document.title, "disktree · /tmp");
  assert.ok(app.innerHTML.includes("panel"));

  // A mosaic answer paints the wrap alone and updates the zoom readout.
  const wrap = withMosaic(document);
  await shim.paintMosaic({ mosaic: "<svg id=\"mosaic\"></svg>", zoom: "1.4×" });
  assert.equal(wrap.innerHTML, '<svg id="mosaic"></svg>');
  assert.equal(document.getElementById("zoom-level").textContent, "1.4×");
  assert.ok(app.innerHTML.includes("panel"), "the rest is untouched");
});

test("wheel notches coalesce into one zoom per flush", async () => {
  const { document, calls } = boot();
  const wrap = withMosaic(document);
  const tile = document.attach("div", { classes: ["g-tile"], parent: wrap });
  const fire = (dy) =>
    document._fire("wheel", {
      target: tile,
      clientX: 500,
      clientY: 350,
      deltaY: dy,
      deltaMode: 0,
      shiftKey: false,
      ...noDefault(),
    });
  fire(-24);
  fire(-24);
  fire(-24);
  await wait(90);
  const zooms = calls
    .filter((c) => c.body && c.body.includes('"zoom"'))
    .map((c) => JSON.parse(c.body).events[0]);
  assert.equal(zooms.length, 1, "three notches, one request");
  assert.ok(Math.abs(zooms[0].factor - 1.15 ** 3) < 1e-6);
});

test("the find field: focus on open, no focus traffic, no loop", async () => {
  const { document, context, calls } = boot();
  const shim = context;
  const frame = {
    html: '<span id="find"><input id="find-input" value=""></span>',
    title: "t",
    busy: false,
    find_open: true,
    find: "",
  };
  await shim.apply(frame);
  const input = document.getElementById("find-input");
  assert.equal(document.activeElement, input, "opening focuses the field");
  const baseline = calls.length;

  // Frames keep coming while the user types; the focused field keeps its
  // own in-flight text over the server's round-trip-behind render.
  input.value = "ta";
  await shim.apply({ ...frame, html: '<input id="find-input" value="t">' });
  const focused = document.getElementById("find-input");
  assert.equal(
    document.activeElement,
    focused,
    "focus survives the swap, in the stub as in the browser",
  );
  assert.equal(focused.value, "ta", "the typist's text survives the swap");
  assert.equal(
    calls.length,
    baseline,
    "no focus or input traffic came out of applying frames",
  );

  // And no focus event ever reaches the wire, by construction.
  assert.equal(
    calls.filter((c) => c.body && c.body.includes("find_focus")).length,
    0,
  );
});

test("typing posts the debounced full text; escape clears", async () => {
  const { document, calls } = boot();
  const input = document.attach("input", { id: "find-input" });
  input.value = "ca";
  document._fire("input", { target: input });
  input.value = "car";
  document._fire("input", { target: input });
  await wait(120);
  const finds = calls
    .filter((c) => c.body && c.body.includes('"find"'))
    .map((c) => JSON.parse(c.body).events[0]);
  assert.equal(finds.length, 1, "two keystrokes, one debounced request");
  assert.equal(finds[0].text, "car");

  document._fire("keydown", { target: input, key: "Escape", ...noDefault() });
  await wait(0);
  const clears = calls
    .filter((c) => c.body && c.body.includes("find_clear"))
    .map((c) => JSON.parse(c.body).events[0].type);
  assert.deepEqual(clears, ["find_clear"]);
});

test("a stale answer is dropped in favour of the newer one", async () => {
  const resolvers = [];
  const { context, document } = boot({
    responses: ({ opts }) => {
      // Polls and other frame GETs answer at once; only input posts park.
      if (!opts || !opts.body) {
        return {
          status: 200,
          json: { html: "boot", title: "t", busy: false, find_open: false, find: "" },
        };
      }
      return new Promise((resolve) => resolvers.push(resolve));
    },
  });
  const shim = context;
  const app = document.getElementById("app");

  const first = shim.send([{ type: "key", key: "c" }]);
  const second = shim.send([{ type: "key", key: "?" }]);
  // The second answers first; then the first arrives — and must be dropped.
  resolvers[1]({ status: 200, json: { html: "second", title: "t", busy: false, find_open: false, find: "" } });
  await second;
  resolvers[0]({ status: 200, json: { html: "first", title: "t", busy: false, find_open: false, find: "" } });
  await first;
  assert.equal(app.innerHTML, "second");
});

test("long-press marks with ctrl; its synthetic click is suppressed", async () => {
  const { document, calls } = boot();
  const wrap = withMosaic(document);
  const tile = document.attach("div", { classes: ["g-tile"], parent: wrap });
  document._fire("touchstart", {
    target: tile,
    touches: [{ clientX: 100, clientY: 100, identifier: 1 }],
  });
  await wait(600);
  document._fire("touchend", { touches: [] });
  // The synthetic click that follows a long-press.
  document._fire("mousedown", {
    target: tile,
    clientX: 100,
    clientY: 100,
    button: 0,
    detail: 1,
    ...noDefault(),
  });
  await wait(0);
  const clicks = calls
    .filter((c) => c.body && c.body.includes('"click"'))
    .map((c) => JSON.parse(c.body).events[0]);
  assert.equal(clicks.length, 1, "the long-press's ctrl-click only");
  assert.equal(clicks[0].ctrl, true);
});

test("the poll loop survives being superseded by pointer traffic", async () => {
  // busy frames poll at 400 ms; a move storm must not stop them.
  const { document, context, calls, frame } = boot({
    responses: ({ url, opts }) => {
      if (opts && opts.body) return { status: 204 };
      return {
        status: 200,
        json: { html: "x", title: "t", busy: true, find_open: false, find: "" },
      };
    },
  });
  const shim = context;
  const wrap = withMosaic(document);
  await shim.apply({ html: "x", title: "t", busy: true, find_open: false, find: "" });

  const pollsBefore = calls.filter((c) => !c.body && c.url.includes("/api/frame")).length;
  // Pointer traffic at ~60 ms for a second, spanning the 400 ms tick.
  const tile = document.attach("div", { classes: ["g-tile"], parent: wrap });
  for (let i = 0; i < 12; i++) {
    document._fire("mousemove", { target: tile, clientX: 100 + i, clientY: 100 });
    frame();
    await wait(90);
  }
  const polls = calls.filter((c) => !c.body && c.url.includes("/api/frame")).length;
  assert.ok(polls > pollsBefore, "the meter kept ticking");
});
