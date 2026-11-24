import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";

/** The largest test window the rig opens, in points. */
export const WINDOW_MAX = Object.freeze({ width: 1280, height: 900 });

/** The smallest test window the rig accepts, in points; a display that cannot hold it is refused. */
export const WINDOW_MIN = Object.freeze({ width: 960, height: 640 });

/** The room the rig leaves between the test window and the edges of a display's visible area. */
export const MARGIN = 20;

/** How far inside a display's visible area the test window opens, from its top left. */
const INSET = 10;

/** The most displays the rig reads, so a bad answer cannot grow without bound. */
const DISPLAYS_MAX = 16;

/** Lists the displays from AppKit, main first, each with its frame, its visible frame (the
 * menu bar and Dock left out) and its backing scale, in points with the origin at the main
 * display's bottom left. Output: the displays; throws when AppKit cannot be read. */
export function displays() {
  const script = "ObjC.import('AppKit'); JSON.stringify(ObjC.unwrap($.NSScreen.screens).map((s) => ({"
    + " x: s.frame.origin.x, y: s.frame.origin.y, width: s.frame.size.width, height: s.frame.size.height,"
    + " visible: { x: s.visibleFrame.origin.x, y: s.visibleFrame.origin.y, width: s.visibleFrame.size.width,"
    + " height: s.visibleFrame.size.height }, scale: s.backingScaleFactor })))";
  const result = spawnSync("/usr/bin/osascript", ["-l", "JavaScript", "-e", script], { encoding: "utf8" });
  assert.equal(result.status, 0, `the display list: ${result.stderr}`);
  return JSON.parse(result.stdout);
}

/** One display as the rig prints it: its visible size and scale. Input: the display.
 * Output: text such as "1024x743@1". */
export function shown(display) {
  return `${Math.floor(display.visible.width)}x${Math.floor(display.visible.height)}@${display.scale}`;
}

/** Picks the test window: on the first 1x display when `scaleOne`, so it never opens on the
 * owner's 2x built-in display, else on the main display, the window is WINDOW_MAX shrunk to
 * that display's visible area less MARGIN, opening INSET inside its top left. Inputs: the
 * displays (main first) and whether only a 1x display may hold it. Output: { width, height,
 * place: "x,y" in points from the main display's top left, display }; throws naming every
 * display when none holds WINDOW_MIN. */
export function testWindow(listed, { scaleOne = false } = {}) {
  assert.ok(Array.isArray(listed) && listed.length > 0 && listed.length <= DISPLAYS_MAX, `a display list: ${JSON.stringify(listed)}`);
  assert.ok(listed[0].x === 0 && listed[0].y === 0, "the main display is listed first");
  const size = (display) => ({
    width: Math.min(WINDOW_MAX.width, Math.floor(display.visible.width) - MARGIN),
    height: Math.min(WINDOW_MAX.height, Math.floor(display.visible.height) - MARGIN),
  });
  const holds = (display) => size(display).width >= WINDOW_MIN.width && size(display).height >= WINDOW_MIN.height;
  const candidates = scaleOne ? listed.filter((display) => display.scale === 1) : [listed[0]];
  const display = candidates.find(holds);
  assert.ok(display, `no ${scaleOne ? "1x " : "main "}display holds a ${WINDOW_MIN.width}x${WINDOW_MIN.height} test window`
    + ` with a ${MARGIN} point margin: ${listed.map(shown).join(", ")}`);
  const { width, height } = size(display);
  const x = display.visible.x + INSET;
  const y = listed[0].height - display.visible.y - display.visible.height + INSET;
  assert.ok(width <= WINDOW_MAX.width && height <= WINDOW_MAX.height);
  assert.ok(INSET + width <= display.visible.width && INSET + height <= display.visible.height);
  return { width, height, place: `${x},${y}`, display };
}
