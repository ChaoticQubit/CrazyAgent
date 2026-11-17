import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { bundle, withPage } from "./browser.mjs";

const fixture = await bundle(new URL("./fixtures/tabs-look.mjs", import.meta.url).pathname);
const style = readFileSync(new URL("../src/style.css", import.meta.url), "utf8");
const settings = readFileSync(new URL("../src/screens/settings/settings.css", import.meta.url), "utf8");
const CLEAR = "rgba(0, 0, 0, 0)";

/**
 * The WCAG 2 contrast ratio of two computed colors. Inputs: two rgb() strings.
 * Output: the ratio, at least 1.
 */
function ratio(first, second) {
  const lum = (color) => {
    const [red, green, blue] = color.match(/[\d.]+/g).slice(0, 3).map((value) => {
      const part = Number(value) / 255;
      return part <= 0.03928 ? part / 12.92 : ((part + 0.055) / 1.055) ** 2.4;
    });
    return 0.2126 * red + 0.7152 * green + 0.0722 * blue;
  };
  const [light, dark] = [lum(first), lum(second)].sort((a, b) => b - a);
  return (light + 0.05) / (dark + 0.05);
}

/**
 * Mounts the tabs and the segmented control in one theme and reads their look.
 * Input: the theme. Output: what `Fixture.look()` returns.
 */
function looks(theme) {
  return withPage(async (page) => {
    await page.evaluate(fixture);
    await page.evaluate(`Fixture.mount(${JSON.stringify(theme)})`);
    return page.evaluate("Fixture.look()");
  });
}

for (const theme of ["light", "dark"]) {
  test(`${theme}: a selected tab, link or button, is ink text at 500 over an ink --primary underline, and the others are --text-2 at 500 with none`, async () => {
    const seen = await looks(theme);
    for (const kind of ["link", "strip"]) {
      assert.deepEqual(seen[kind].selected, { color: seen.tokens.text, weight: "500", background: CLEAR, underline: seen.tokens.primary, edge: "none" }, kind);
      assert.deepEqual(seen[kind].other, { color: seen.tokens["text-2"], weight: "500", background: CLEAR, underline: CLEAR, edge: "none" }, kind);
    }
  });

  test(`${theme}: a segmented control is a hairline --border group on --surface, its chosen segment ink text at 500 on --surface-3 inside an ink --primary edge`, async () => {
    const seen = await looks(theme);
    assert.deepEqual(seen.group, { border: seen.tokens.border, background: seen.tokens.surface });
    assert.deepEqual(seen.segment.selected, { color: seen.tokens.text, weight: "500", background: seen.tokens["surface-3"], underline: seen.segment.selected.underline, edge: `${seen.tokens.primary} 0px 0px 0px 1px inset` });
    assert.deepEqual(seen.segment.other, { color: seen.tokens["text-2"], weight: "500", background: CLEAR, underline: seen.segment.other.underline, edge: "none" });
  });

  test(`${theme}: the chosen segment's edge is a non-color cue of at least 3:1 against its own fill and the group around it`, async () => {
    const seen = await looks(theme);
    for (const fill of ["surface-3", "surface"]) {
      const value = ratio(seen.tokens.primary, seen.tokens[fill]);
      assert.ok(value >= 3, `--primary on --${fill}: ${value.toFixed(2)}`);
    }
  });
}

test("the tab and segmented rules name no accent token, no literal weight and no literal font size", () => {
  const rules = [style, settings].flatMap((sheet) => sheet.split("\n")).filter((line) => /^\.(?:tabs?\b|segmented)|^\.\S+ \.segmented/.test(line));
  assert.ok(rules.length >= 10, `found ${rules.length} tab and segmented rules`);
  assert.deepEqual(rules.filter((line) => /var\(--(?:accent|on-accent)|font-weight:\s*[0-9]|font-size:\s*[0-9]/.test(line)), []);
  assert.ok(rules.includes(".pool-order-pick .segmented button, .pool-ctl .segmented button { min-height: 22px; padding: 0 7px; font-size: var(--font-secondary); }"));
});
