import assert from "node:assert/strict";
import { bundle, withPage } from "../browser.mjs";

const fixture = await bundle(new URL("./settings-app-tabs.mjs", import.meta.url).pathname);

export const CONTRAST_MIN = 4.5;

/** Runs a check on the real Settings page on one tab. Inputs: the tab, the scene, the check
 * and the theme. Output: the check's value. */
export function onTab(tab, scene, check, theme = "light") {
  return withPage(async (page) => {
    await page.send("Emulation.setDeviceMetricsOverride", { width: 1180, height: 900, deviceScaleFactor: 1, mobile: false });
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
  await page.evaluate("Fixture.settle(120)");
}

/** Walks the tab with real Tab keys from the last Settings link, or in a user menu dialog
 * from its Close button. Inputs: the page and how many stops. Output: each stop's name. */
export async function walk(page, stops) {
  await page.evaluate("(document.querySelector('dialog[open] .dialog-head button:last-child') ?? [...document.querySelectorAll('.settings-nav a')].at(-1)).focus()");
  const seen = [];
  for (let at = 0; at < stops; at += 1) {
    for (const type of ["keyDown", "keyUp"]) await page.send("Input.dispatchKeyEvent", { type, key: "Tab", code: "Tab", windowsVirtualKeyCode: 9 });
    seen.push(await page.evaluate("Fixture.focused()"));
  }
  return seen;
}

/** Reads what every layout test asks of a tab. Input: the page. Output: the top line, the
 * sections and the page's words. */
export async function shown(page) {
  return { top: await page.evaluate("Fixture.top()"), sections: await page.evaluate("Fixture.sections()"), says: await page.evaluate("Fixture.text()") };
}

/** Asserts the contrast of each selector. Inputs: the page and the selectors. */
export async function readable(page, selectors) {
  for (const selector of selectors) {
    assert.equal(await page.evaluate(`document.querySelector(${JSON.stringify(selector)}) !== null`), true, `${selector} is on the page`);
    const ratio = await page.evaluate(`Fixture.contrast(${JSON.stringify(selector)})`);
    assert.ok(ratio >= CONTRAST_MIN, `${selector}: ${ratio}`);
  }
}
