import assert from "node:assert/strict";
import { bundle, withPage } from "./browser.mjs";

const fixture = await bundle(new URL("./fixtures/settings-agents-layout.mjs", import.meta.url).pathname);
const TAB_KEY = { key: "Tab", code: "Tab", windowsVirtualKeyCode: 9 };

export const CONTRAST_MIN = 4.5;

/** Runs a check on the real Settings page on one tab. Inputs: the tab, the scene, the check,
 * the window width and the theme. Output: the check's value. */
export function onTab(tab, scene, check, width = 1180, theme = "light") {
  return withPage(async (page) => {
    await page.send("Emulation.setDeviceMetricsOverride", { width, height: 900, deviceScaleFactor: 1, mobile: false });
    await page.evaluate(fixture);
    await page.evaluate(`Fixture.mount(${JSON.stringify(tab)}, ${JSON.stringify(scene)}, ${JSON.stringify(theme)})`);
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

/** Double-presses an element where it first stood with the real mouse, as a hurried owner does.
 * Inputs: the page, the selector and its words, if more than one element matches. */
export async function doublePress(page, selector, wanted = null) {
  const at = await page.evaluate(`Fixture.boxOf(${JSON.stringify(selector)}, ${JSON.stringify(wanted)})`);
  assert.notEqual(at, null, `${selector} ${wanted ?? ""} is on the page`);
  for (const clickCount of [1, 2]) {
    for (const type of ["mousePressed", "mouseReleased"]) await page.send("Input.dispatchMouseEvent", { type, x: at.x, y: at.y, button: "left", clickCount });
  }
  await page.evaluate("Fixture.settle(80)");
}

/** Presses and releases one real key. Inputs: the page and the key event's fields. */
export async function stroke(page, key) {
  for (const type of ["keyDown", "keyUp"]) await page.send("Input.dispatchKeyEvent", { type, ...key });
  await page.evaluate("Fixture.settle(80)");
}

/** Types into a field with real input, replacing what it holds. Inputs: the page, the field's
 * selector and the words. */
export async function typeInto(page, selector, words) {
  await page.evaluate(`(() => { const field = document.querySelector(${JSON.stringify(selector)}); field.focus(); field.select(); })()`);
  await page.send("Input.insertText", { text: words });
  await page.evaluate("Fixture.settle(80)");
}

/** Walks the tab with real Tab keys from the last settings link. Inputs: the page and the
 * number of presses. Output: the name of each stop. */
export async function tabStops(page, steps) {
  await page.evaluate("[...document.querySelectorAll('.settings-nav a')].at(-1).focus()");
  const seen = [];
  for (let at = 0; at < steps; at += 1) {
    for (const type of ["keyDown", "keyUp"]) await page.send("Input.dispatchKeyEvent", { type, ...TAB_KEY });
    seen.push((await page.evaluate("Fixture.focused()")).name);
  }
  return seen;
}

/** Reads the contrast of each selector. Inputs: the page and the selectors. Output: a
 * selector to ratio map. */
export async function contrasts(page, selectors) {
  const ratios = {};
  for (const selector of selectors) ratios[selector] = await page.evaluate(`Fixture.contrast(${JSON.stringify(selector)})`);
  return ratios;
}

export const read = (page) => page.evaluate("Fixture.sections()");
export const say = (page) => page.evaluate("Fixture.text()");
export const top = (page) => page.evaluate("Fixture.top()");
export const dialog = (page) => page.evaluate("Fixture.dialog()");
export const commands = (page) => page.evaluate("Fixture.commands()");
export const toasts = (page) => page.evaluate("Fixture.toasts()");
export const count = (page, selector) => page.evaluate(`Fixture.count(${JSON.stringify(selector)})`);
export const argsOf = (page, command) => page.evaluate(`Fixture.argsOf(${JSON.stringify(command)})`);
