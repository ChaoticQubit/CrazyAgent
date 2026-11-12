import assert from "node:assert/strict";
import test from "node:test";
import { scroller, shifted, windowOff } from "./scroll-points.mjs";

/** One entry of a scroll chain as scrollState reads it. Inputs: the name, the overflow-y
 * style, how far it can scroll and where it is scrolled to. Output: the entry. */
function box(name, overflowY = "visible", room = 0, top = 0, left = 0) {
  return { name, overflowY, room, top, left };
}

/** A window scroll state over a shell: html, body and the root fixed, the main panel and a
 * paragraph inside it. Input: fields to change. Output: the state. */
function state({ x = 0, y = 0, html = 0, main = 0, inner = "p" } = {}) {
  return { window: { x, y }, chain: [box("html", "hidden", 0, html), box("body", "hidden"), box("div#root", "hidden"), box("main.main", "auto", 500, main), box(inner)] };
}

test("the scroller under the pointer is the deepest box the owner can scroll that has room to move", () => {
  assert.equal(scroller(state().chain), 3);
  assert.equal(scroller([...state().chain.slice(0, 4), box("div.windowed", "auto", 0), box("p")]), 3);
  assert.equal(scroller([...state().chain.slice(0, 4), box("div.windowed", "scroll", 80), box("p")]), 4);
  assert.equal(scroller([box("html", "hidden", 900), box("body", "hidden"), box("aside.sidebar", "auto", 1)]), -1);
  assert.equal(scroller([]), -1);
});

test("a shift names the window and every box that moved, apart from the kept one", () => {
  assert.deepEqual(shifted(state(), state(), -1), []);
  assert.deepEqual(shifted(state(), state({ y: 40 }), -1), ["the window"]);
  assert.deepEqual(shifted(state(), state({ html: 40, main: 240 }), -1), ["html", "main.main"]);
  assert.deepEqual(shifted(state(), state({ main: 240 }), 3), []);
  assert.deepEqual(shifted(state(), state({ html: 40, main: 240 }), 3), ["html"]);
});

test("a shift compares only the boxes both reads share from the top, so new content under the pointer is not a move", () => {
  assert.deepEqual(shifted(state(), state({ inner: "div.card" }), -1), []);
  assert.deepEqual(shifted(state(), { ...state({ main: 240 }), chain: state({ main: 240 }).chain.slice(0, 4) }, -1), ["main.main"]);
});

test("the window is off its place when it or html, body or the root is scrolled at all", () => {
  assert.deepEqual(windowOff(state({ main: 240 })), []);
  assert.deepEqual(windowOff(state({ x: 3 })), ["the window"]);
  assert.deepEqual(windowOff(state({ html: 12 })), ["html"]);
  const root = state();
  root.chain[2] = box("div#root", "hidden", 0, 0, 8);
  assert.deepEqual(windowOff(root), ["div#root"]);
});
