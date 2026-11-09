import assert from "node:assert/strict";
import { withPage } from "./browser.mjs";

export const CONTRAST_MIN = 4.5;

/** Makes the runner for one settings tab's fixture. Input: the bundled fixture. Output: a
 * function taking the scene, the check, the window width and the theme, returning the check's
 * value. */
export function tabRunner(fixture) {
  return (scene, check, width = 1180, theme = "light") => withPage(async (page) => {
    await page.send("Emulation.setDeviceMetricsOverride", { width, height: 900, deviceScaleFactor: 1, mobile: false });
    await page.evaluate(fixture);
    await page.evaluate(`Fixture.mount(${JSON.stringify(scene)}, ${JSON.stringify(theme)})`);
    return check(page);
  });
}

/** Presses an element with the real mouse. Inputs: the page, the selector and its words, if
 * more than one element matches. */
export async function press(page, selector, wanted = null) {
  const at = await page.evaluate(`Fixture.boxOf(${JSON.stringify(selector)}, ${JSON.stringify(wanted)})`);
  assert.notEqual(at, null, `${selector} ${wanted ?? ""} is on the page`);
  for (const type of ["mousePressed", "mouseReleased"]) await page.send("Input.dispatchMouseEvent", { type, x: at.x, y: at.y, button: "left", clickCount: 1 });
  await page.evaluate("Fixture.settle(80)");
}

/** Presses an element twice with the real mouse, with nothing waited for between the presses,
 * as a double click does. Inputs: the page, the selector and its words, if more than one
 * element matches. */
export async function doublePress(page, selector, wanted = null) {
  const at = await page.evaluate(`Fixture.boxOf(${JSON.stringify(selector)}, ${JSON.stringify(wanted)})`);
  assert.notEqual(at, null, `${selector} ${wanted ?? ""} is on the page`);
  for (const clickCount of [1, 2]) {
    for (const type of ["mousePressed", "mouseReleased"]) await page.send("Input.dispatchMouseEvent", { type, x: at.x, y: at.y, button: "left", clickCount });
  }
  await page.evaluate("Fixture.settle(80)");
}

/** Walks focus with real Tab keys from the last Settings list link. Inputs: the page and the
 * number of keys. Output: each focused control's name. */
export async function tabWalk(page, keys) {
  await page.evaluate("[...document.querySelectorAll('.settings-nav a')].at(-1).focus()");
  const seen = [];
  for (let at = 0; at < keys; at += 1) {
    for (const type of ["keyDown", "keyUp"]) await page.send("Input.dispatchKeyEvent", { type, key: "Tab", code: "Tab", windowsVirtualKeyCode: 9 });
    seen.push((await page.evaluate("Fixture.focused()")).name);
  }
  return seen;
}
