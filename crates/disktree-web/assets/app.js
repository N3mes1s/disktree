/* disktree-web: the browser shim.
 *
 * The server owns the application: every key, click and wheel notch is posted
 * here, and the answer is a fresh frame of HTML to swap in. This file only
 * forwards input and keeps the find field's focus honest — there is no client
 * state about the disk by design.
 */
"use strict";

const app = document.getElementById("app");
const token = new URLSearchParams(location.search).get("token") || "";
const suffix = token ? "?token=" + encodeURIComponent(token) : "";

/* Out-of-order responses must not clobber newer frames. */
let sequence = 0;
let busy = false;
let lastSentSize = [0, 0];

/* Mosaic size is reported with every batch: the server lays out in pixels. */
function mosaicSize() {
  const wrap = document.getElementById("mosaic-wrap");
  if (!wrap) return [0, 0];
  const rect = wrap.getBoundingClientRect();
  return [Math.round(rect.width), Math.round(rect.height)];
}

async function send(events) {
  const [w, h] = mosaicSize();
  if (w > 0) lastSentSize = [w, h];
  const ticket = ++sequence;
  try {
    const response = await fetch("/api/input" + suffix, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ w, h, events }),
    });
    if (response.status === 204) return;
    if (!response.ok) return;
    const answer = await response.json();
    if (ticket !== sequence) return;
    if (answer.html !== undefined) apply(answer);
    else if (answer.mosaic !== undefined) paintMosaic(answer);
  } catch (_) {
    /* Dropped answers are safe: the next event or poll repaints. */
  }
}

/* A gesture moved the camera only: swap the mosaic, update the zoom
 * readout, leave the rest of the page alone. */
function paintMosaic(answer) {
  const wrap = document.getElementById("mosaic-wrap");
  if (!wrap) return;
  hideTip();
  wrap.innerHTML = answer.mosaic;
  const zoom = document.getElementById("zoom-level");
  if (zoom) zoom.textContent = answer.zoom || "";
  /* Mosaic-only answers skip apply()'s bookkeeping, so the size correction
   * lives here too: chrome appearing or disappearing reshapes the wrap,
   * and the layout must be told or the mosaic shows margins. */
  const [w, h] = mosaicSize();
  if (w > 0 && (w !== lastSentSize[0] || h !== lastSentSize[1])) {
    lastSentSize = [w, h];
    send([{ type: "size", w, h }]);
  }
}

async function poll() {
  const [w, h] = mosaicSize();
  const ticket = ++sequence;
  /* Every exit reschedules: a poll superseded by pointer traffic (204s bump
   * the sequence) that didn't would stop the meter and the progress forever. */
  try {
    const response = await fetch(
      "/api/frame" + suffix + (suffix ? "&" : "?") + "w=" + w + "&h=" + h);
    if (response.ok) {
      const frame = await response.json();
      if (ticket === sequence) apply(frame);
    }
  } catch (_) {
    /* The server went away; the next tick tries again. */
  }
  schedule();
}

function apply(frame) {
  hideTip();
  const old = document.getElementById("find-input");
  const findWasFocused = old && document.activeElement === old;
  /* The server is a round-trip behind the typist; its rendered value would
   * swallow the last few keystrokes, so the focused field keeps its own
   * text. The server's copy converges, since every change is sent in full. */
  const liveText = findWasFocused ? old.value : null;
  const liveCaret = findWasFocused ? old.selectionStart : 0;
  app.innerHTML = frame.html;
  document.title = frame.title;
  busy = frame.busy;
  if (sheetOpen) document.getElementById("panel")?.classList.add("open");
  /* Focus follows the transition into find-open, never the steady state:
   * grabbing it on every frame would bounce the keyboard back to the field
   * after every click. */
  const opening = frame.find_open && !old;
  if (opening || findWasFocused) {
    const input = document.getElementById("find-input");
    if (input) {
      if (liveText !== null) input.value = liveText;
      input.focus();
      input.setSelectionRange(liveCaret, liveCaret);
    }
  }
  /* The container exists only once the explore screen is on the page; the
   * first frames are laid out blind, so correct the size as soon as it can
   * be measured. */
  const [w, h] = mosaicSize();
  if (w > 0 && (w !== lastSentSize[0] || h !== lastSentSize[1])) {
    lastSentSize = [w, h];
    send([{ type: "size", w, h }]);
    return;
  }
  schedule();
}

let timer = null;
function schedule() {
  clearTimeout(timer);
  /* Live numbers while a scan or removal runs; a slow tick otherwise so the
   * free-space meter stays honest. */
  timer = setTimeout(poll, busy ? 400 : 4000);
}

/* ── clicks on server-rendered controls ─────────────────────────────── */

/* The details sheet is client-side chrome; it survives frame swaps. */
let sheetOpen = false;

document.addEventListener("click", (event) => {
  if (event.target.closest("#sheet-toggle")) {
    sheetOpen = true;
    document.getElementById("panel")?.classList.add("open");
    return;
  }
  if (event.target.closest("#panel-close")) {
    sheetOpen = false;
    document.getElementById("panel")?.classList.remove("open");
    return;
  }
  const control = event.target.closest("[data-ev]");
  if (!control || control.disabled) return;
  if (control.closest("#mosaic-wrap")) return;
  const action = JSON.parse(control.dataset.ev);
  /* A crumb menu opens at its crumb; measure it now, the server cannot. */
  if (action.type === "menu") {
    const rect = control.getBoundingClientRect();
    action.x = Math.round(rect.left);
    action.y = Math.round(rect.bottom + 4);
  }
  event.preventDefault();
  event.stopPropagation();
  send([action]);
});

/* ── the mosaic: coordinates, not elements ──────────────────────────── */

function local(event) {
  const rect = document.getElementById("mosaic-wrap").getBoundingClientRect();
  return [Math.round(event.clientX - rect.left),
          Math.round(event.clientY - rect.top)];
}

/* The ring and the tooltip are local; the server still tracks the pointer
 * so Space, X and Enter act on the tile under it. That earns an empty 204,
 * not a frame, so moves can stay frequent without repainting anything. */
let pendingMove = null;
let lastMoveSent = 0;
document.addEventListener("mousemove", (event) => {
  if (!event.target.closest || !event.target.closest("#mosaic-wrap")) return;
  pendingMove = event;
});
(function movePump() {
  const now = performance.now();
  if (pendingMove && now - lastMoveSent > 60) {
    const [x, y] = local(pendingMove);
    pendingMove = null;
    lastMoveSent = now;
    send([{ type: "move", x, y }]);
  }
  requestAnimationFrame(movePump);
})();

document.addEventListener("mouseleave", (event) => {
  if (event.target && event.target.id === "mosaic-wrap") {
    send([{ type: "leave" }]);
  }
}, true);

document.addEventListener("mousedown", (event) => {
  if (!event.target.closest || !event.target.closest("#mosaic-wrap")) return;
  if (event.target.closest("#tooltip")) return;
  /* The synthetic click after a long-press. */
  if (performance.now() - longPressAt < 700) return;
  event.preventDefault();
  const [x, y] = local(event);
  send([{
    type: "click",
    x, y,
    button: event.button,
    ctrl: event.ctrlKey || event.metaKey,
    count: event.detail,
  }]);
});

/* Wheel notches coalesce into one zoom factor per flush: a trackpad's
 * flurry is one gesture, not twenty requests. */
let wheelAcc = null;
document.addEventListener("wheel", (event) => {
  if (!event.target.closest || !event.target.closest("#mosaic-wrap")) return;
  event.preventDefault();
  hideTip();
  const [x, y] = local(event);
  const lines = event.deltaMode === 1 ? event.deltaY : event.deltaY / 24;
  const factor = lines < 0 ? 1.15 : 1 / 1.15;
  if (!wheelAcc) {
    wheelAcc = { x, y, factor: 1, shift: event.shiftKey, timer: null };
  }
  wheelAcc.factor *= factor;
  wheelAcc.x = x;
  wheelAcc.y = y;
  if (event.shiftKey) wheelAcc.shift = true;
  if (!wheelAcc.timer) {
    wheelAcc.timer = setTimeout(() => {
      const batch = wheelAcc;
      wheelAcc = null;
      if (batch.shift) {
        /* Scroll up pans the content up: a positive factor means up. */
        send([{ type: "pan", dx: 0, dy: (batch.factor - 1) * 260 }]);
      } else {
        send([{ type: "zoom", x: batch.x, y: batch.y, factor: batch.factor }]);
      }
    }, 55);
  }
}, { passive: false });

/* Middle click would otherwise start the browser's auto-scroll. */
document.addEventListener("auxclick", (event) => {
  if (event.target.closest && event.target.closest("#mosaic-wrap")) {
    event.preventDefault();
  }
});

/* ── touch: tap selects, long-press marks, drag pans, pinch zooms ──────
 *
 * Tap arrives as the browser's synthetic click, so only the gestures the
 * mouse cannot express live here. touch-action:none on the wrap keeps the
 * browser from eating them.
 */

/* After a long-press fires, the tap's synthetic click must not also act. */
let longPressAt = 0;

let gesture = null;
document.addEventListener("touchstart", (event) => {
  if (!event.target.closest || !event.target.closest("#mosaic-wrap")) return;
  if (event.touches.length === 1) {
    const t = event.touches[0];
    gesture = {
      mode: "maybe", moved: false,
      x: t.clientX, y: t.clientY, lastX: t.clientX, lastY: t.clientY,
      timer: setTimeout(() => {
        if (gesture && gesture.mode === "maybe" && !gesture.moved) {
          const [x, y] = local({ clientX: gesture.x, clientY: gesture.y });
          /* Long-press is ctrl-click: mark without moving the selection. */
          send([{ type: "click", x, y, button: 0, ctrl: true, count: 1 }]);
          longPressAt = performance.now();
          gesture.mode = "done";
        }
      }, 450),
    };
  } else if (event.touches.length === 2) {
    if (gesture && gesture.timer) clearTimeout(gesture.timer);
    const [a, b] = event.touches;
    gesture = {
      mode: "pinch",
      dist: Math.hypot(a.clientX - b.clientX, a.clientY - b.clientY),
      lastSent: 0,
    };
  }
}, { passive: true });

document.addEventListener("touchmove", (event) => {
  if (!gesture) return;
  if (gesture.mode === "pinch" && event.touches.length === 2) {
    event.preventDefault();
    const [a, b] = event.touches;
    const dist = Math.hypot(a.clientX - b.clientX, a.clientY - b.clientY);
    const now = performance.now();
    if (gesture.dist > 0 && now - gesture.lastSent > 50) {
      const factor = dist / gesture.dist;
      gesture.dist = dist;
      gesture.lastSent = now;
      const mid = { clientX: (a.clientX + b.clientX) / 2,
                    clientY: (a.clientY + b.clientY) / 2 };
      const [x, y] = local(mid);
      send([{ type: "zoom", x, y, factor }]);
    }
    return;
  }
  if (event.touches.length !== 1) return;
  const t = event.touches[0];
  const dx = t.clientX - gesture.lastX;
  const dy = t.clientY - gesture.lastY;
  if (!gesture.moved && Math.hypot(t.clientX - gesture.x, t.clientY - gesture.y) > 10) {
    gesture.moved = true;
    gesture.mode = "pan";
    clearTimeout(gesture.timer);
  }
  if (gesture.mode === "pan" && (dx || dy)) {
    event.preventDefault();
    gesture.lastX = t.clientX;
    gesture.lastY = t.clientY;
    gesture.dx = (gesture.dx || 0) + dx;
    gesture.dy = (gesture.dy || 0) + dy;
    if (!gesture.flushing) {
      gesture.flushing = true;
      setTimeout(() => {
        if (!gesture) return;
        const batch = { dx: gesture.dx || 0, dy: gesture.dy || 0 };
        gesture.dx = 0;
        gesture.dy = 0;
        gesture.flushing = false;
        if (batch.dx || batch.dy) send([{ type: "pan", ...batch }]);
      }, 55);
    }
  }
}, { passive: false });

document.addEventListener("touchend", () => {
  if (gesture && gesture.timer) clearTimeout(gesture.timer);
  gesture = null;
});

/* A long-press already acted; the tap's synthetic click must not too. */
document.addEventListener("contextmenu", (event) => {
  if (event.target.closest && event.target.closest("#mosaic-wrap")) {
    event.preventDefault();
  }
});

/* ── the tooltip, built here: the tiles carry what it needs ───────────
 *
 * Everything is set with textContent: a file named like markup stays text.
 */
let tipTile = null;

function hideTip() {
  tipTile = null;
  document.getElementById("tooltip")?.remove();
}

function tipEl(tag, cls, text) {
  const el = document.createElement(tag);
  if (cls) el.className = cls;
  if (text !== null) el.textContent = text;
  return el;
}

function showTip(tile, x, y) {
  const d = tile.dataset;
  if (!d.size) return; /* the merged "+N more" tail has nothing to add */
  hideTip();
  tipTile = tile;
  const tip = tipEl("div", "", null);
  tip.id = "tooltip";

  const name = tipEl("div", "tip-name", null);
  name.appendChild(tipEl("span", "tip-icon", d.dir === "1" ? "▣" : "▢"));
  name.appendChild(tipEl("b", "", d.name));
  tip.appendChild(name);
  if (d.tipPath) tip.appendChild(tipEl("div", "tip-path", d.tipPath));

  const size = tipEl("div", "tip-size", null);
  size.appendChild(tipEl("span", "tip-bytes", d.size));
  size.appendChild(tipEl("span", "mono", d.bar));
  size.appendChild(tipEl("span", "dim", d.percent));
  tip.appendChild(size);
  tip.appendChild(tipEl("div", "dim", d.meta));

  const chips = tipEl("div", "chips", null);
  if (d.hidden === "1") chips.appendChild(tipEl("span", "chip chip-dim", "Hidden"));
  if (d.marked === "1") chips.appendChild(tipEl("span", "chip chip-danger", "Marked for removal"));
  if (d.covered) chips.appendChild(tipEl("span", "chip chip-dim", "Inside marked " + d.covered));
  if (chips.children.length) tip.appendChild(chips);

  tip.appendChild(tipEl("div", "dim small",
    d.dir === "1" ? "space mark · enter open" : "space mark"));
  document.body.appendChild(tip);
  positionTip(x, y);
}

function positionTip(x, y) {
  const tip = document.getElementById("tooltip");
  if (!tip) return;
  const gap = 12;
  const w = tip.offsetWidth, h = tip.offsetHeight;
  tip.style.left = Math.min(x + gap, window.innerWidth - w - 4) + "px";
  tip.style.top = Math.min(y + gap, window.innerHeight - h - 4) + "px";
}

document.addEventListener("mouseover", (event) => {
  const tile = event.target.closest
    ? event.target.closest(".g-tile") : null;
  if (tile === tipTile) return;
  if (!tile) { hideTip(); return; }
  showTip(tile, event.clientX, event.clientY);
});

document.addEventListener("mousemove", (event) => {
  if (tipTile) positionTip(event.clientX, event.clientY);
});

document.addEventListener("mousedown", hideTip);
document.addEventListener("wheel", hideTip, { passive: true });
document.addEventListener("keydown", hideTip);
document.addEventListener("touchstart", hideTip, { passive: true });

/* ── keyboard ───────────────────────────────────────────────────────── */

const KEY_NAMES = {
  " ": "space",
  Enter: "enter",
  Escape: "escape",
  Tab: "tab",
  ArrowLeft: "left",
  ArrowRight: "right",
  ArrowUp: "up",
  ArrowDown: "down",
  Home: "home",
  End: "end",
};

document.addEventListener("keydown", (event) => {
  const inFind = event.target && event.target.id === "find-input";
  if (inFind) {
    if (event.key === "Escape") {
      event.preventDefault();
      event.target.blur();
      send([{ type: "find_clear" }]);
    } else if (event.key === "Enter") {
      event.preventDefault();
      event.target.blur();
      send([{ type: "find_apply" }]);
    }
    return;
  }
  if (event.target && /^(INPUT|TEXTAREA|SELECT)$/.test(event.target.tagName)) {
    return;
  }

  /* Enter or space on a focused button fires its click; sending the key too
   * would act twice. */
  if ((event.key === "Enter" || event.key === " ")
    && event.target.closest && event.target.closest("button, [data-ev]")) {
    return;
  }

  let key = event.key;
  if (key === "Backspace") key = "backspace";
  else if (KEY_NAMES[key]) key = KEY_NAMES[key];
  else if (key.length === 1) key = key.toLowerCase();
  else if (!/^F\d+$/.test(key)) return;

  /* Keys the app owns must not scroll, quick-find or move focus. */
  const owned = /^[a-z0-9\[\]\/\-\+=?!xctrgdipm]$/i.test(key)
    || ["space", "enter", "backspace", "escape", "tab",
        "left", "right", "up", "down", "home", "end"].includes(key);
  if (!owned) return;
  if ((event.ctrlKey || event.metaKey) && key !== "escape") return;
  event.preventDefault();
  send([{ type: "key", key, ctrl: event.ctrlKey, shift: event.shiftKey }]);
});

/* ── the find field ─────────────────────────────────────────────────── */

let findTimer = null;
document.addEventListener("input", (event) => {
  if (!event.target || event.target.id !== "find-input") return;
  const text = event.target.value;
  clearTimeout(findTimer);
  findTimer = setTimeout(() => send([{ type: "find", text }]), 60);
});

/* Focus is client-side chrome: the server opens the field on "/" and
 * closes it on enter/esc, and never hears about focus at all — a focus
 * round trip per frame loops forever against the swaps. Clicking the
 * mosaic hands the keyboard back to the app. */
document.addEventListener("mousedown", (event) => {
  if (event.target.closest && event.target.closest("#mosaic-wrap")
    && document.activeElement && document.activeElement.id === "find-input") {
    document.activeElement.blur();
  }
}, true);

/* ── the panel's drag handle ────────────────────────────────────────── */

let dragging = false;
let dragThrottle = false;
document.addEventListener("mousedown", (event) => {
  if (event.target && event.target.id === "panel-handle") {
    dragging = true;
    event.preventDefault();
    if (event.detail >= 2) {
      send([{ type: "panel", px: 368 }]);
      dragging = false;
    }
  }
});

document.addEventListener("mousemove", (event) => {
  if (!dragging) return;
  const px = window.innerWidth - event.clientX;
  if (!dragThrottle) {
    dragThrottle = true;
    setTimeout(() => { dragThrottle = false; }, 50);
    send([{ type: "panel", px: Math.round(px) }]);
  }
});

document.addEventListener("mouseup", () => { dragging = false; });

/* ── resize ─────────────────────────────────────────────────────────── */

let resizeTimer = null;
window.addEventListener("resize", () => {
  clearTimeout(resizeTimer);
  resizeTimer = setTimeout(() => {
    const [w, h] = mosaicSize();
    send([{ type: "size", w, h }]);
  }, 120);
});

poll();
