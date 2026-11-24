import assert from "node:assert/strict";
import { test } from "node:test";
import { MARGIN, WINDOW_MAX, WINDOW_MIN, shown, testWindow } from "./test-window.mjs";

/** A display as AppKit lists it, with a menu bar of `bar` points taken from its visible
 * frame. Inputs: origin, size, scale and bar. Output: the display. */
function display(x, y, width, height, scale, bar = 25) {
  return { x, y, width, height, scale, visible: { x, y, width, height: height - bar } };
}

test("a large display gets the largest window, opened inside its visible area", () => {
  const picked = testWindow([display(0, 0, 1920, 1080, 1)]);
  assert.deepEqual([picked.width, picked.height], [WINDOW_MAX.width, WINDOW_MAX.height]);
  assert.equal(picked.place, `40,${25 + 40}`);
});

test("a display smaller than the largest window gets its visible area less the margin", () => {
  const picked = testWindow([display(0, 0, 1100, 840, 1)]);
  assert.deepEqual([picked.width, picked.height], [1100 - MARGIN, 840 - 25 - MARGIN]);
});

test("a display whose visible area cannot hold the smallest window is refused by its size", () => {
  assert.throws(() => testWindow([display(0, 0, 1024, 768, 1)]), /1024x743@1/);
  const small = WINDOW_MIN.height + MARGIN + 25;
  assert.equal(testWindow([display(0, 0, 1024, small, 1)]).height, WINDOW_MIN.height);
});

test("only a 1x display holds the window when asked, and its place counts from the main display's top", () => {
  const retina = display(0, 0, 1512, 982, 2);
  const outside = display(1512, -98, 1920, 1080, 1, 0);
  const picked = testWindow([retina, outside], { scaleOne: true });
  assert.equal(picked.display, outside);
  assert.equal(picked.place, `${1512 + 40},${982 - (-98) - 1080 + 40}`);
  assert.throws(() => testWindow([retina], { scaleOne: true }), /no 1x display/);
  assert.equal(testWindow([retina, outside]).display, retina);
});

test("a display list prints each visible size and scale", () => {
  assert.equal(shown(display(0, 0, 1024, 768, 1)), "1024x743@1");
});
