import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { config as masterSwitch } from "./master-switch.wdio.conf.mjs";

const WINDOW = { width: 1280, height: 900, margin: 60 };

/** Finds where the test window opens: 40 points inside the first 1x display that holds it
 * with a margin, read from AppKit, so it never opens on the owner's 2x built-in display.
 * Output: "x,y" in points from the main display's top left; throws when no display fits. */
function trialPlace() {
  const script = "ObjC.import('AppKit'); JSON.stringify(ObjC.unwrap($.NSScreen.screens).map((s) => ({ x: s.frame.origin.x,"
    + " y: s.frame.origin.y, width: s.frame.size.width, height: s.frame.size.height, scale: s.backingScaleFactor })))";
  const result = spawnSync("/usr/bin/osascript", ["-l", "JavaScript", "-e", script], { encoding: "utf8" });
  assert.equal(result.status, 0, `the display list: ${result.stderr}`);
  const listed = JSON.parse(result.stdout);
  assert.ok(listed.length > 0 && listed[0].x === 0 && listed[0].y === 0, "the main display is listed first");
  const fits = listed.find((each) => each.scale === 1 && each.width >= WINDOW.width + WINDOW.margin
    && each.height >= WINDOW.height + WINDOW.margin);
  assert.ok(fits, "a 1x display holds the test window; the built-in Retina is never used");
  return `${fits.x + 40},${listed[0].height - fits.y - fits.height + 40}`;
}

const [[service, options]] = masterSwitch.services;

export const config = {
  ...masterSwitch,
  specs: [fileURLToPath(new URL("native-terminal.e2e.mjs", import.meta.url))],
  services: [[service, { ...options, env: { ...options.env, CRAZYAGENT_E2E_WINDOW_AT: trialPlace() } }]],
};
