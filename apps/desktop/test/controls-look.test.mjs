import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { bundle, withPage } from "./browser.mjs";

const fixture = await bundle(new URL("./fixtures/controls-look.mjs", import.meta.url).pathname);
const style = readFileSync(new URL("../src/style.css", import.meta.url), "utf8");

/**
 * Mounts the form controls in one theme and reads their look. Input: the theme.
 * Output: what `Fixture.look()` returns.
 */
function looks(theme) {
  return withPage(async (page) => {
    await page.evaluate(fixture);
    await page.evaluate(`Fixture.mount(${JSON.stringify(theme)})`);
    return page.evaluate("Fixture.look()");
  });
}

for (const theme of ["light", "dark"]) {
  test(`${theme}: inputs and selects keep the 3:1 --border-input, labels sit at 500 and help takes the 12/16 step in --text-3`, async () => {
    const seen = await looks(theme);
    assert.deepEqual([seen.input, seen.select], [seen.tokens["border-input"], seen.tokens["border-input"]]);
    assert.equal(seen.label, "500");
    assert.deepEqual(seen.helper, { color: seen.tokens["text-3"], size: "12px", line: "16px" });
    assert.deepEqual(seen.error, { color: seen.tokens.danger, size: "12px" });
  });

  test(`${theme}: a switch is a --surface-2 track with a --text-3 knob off, and solid --primary with an --on-primary knob on`, async () => {
    const seen = await looks(theme);
    assert.deepEqual(seen.off, { track: seen.tokens["surface-2"], border: seen.tokens["border-input"], knob: seen.tokens["text-3"] });
    assert.deepEqual(seen.on, { track: seen.tokens.primary, border: seen.tokens.primary, knob: seen.tokens["on-primary"] });
  });

  test(`${theme}: a checkbox and a radio take --primary, and a chosen radio card is an ink edge on --surface-3 with its title at 500`, async () => {
    const seen = await looks(theme);
    assert.deepEqual([seen.checkbox, seen.radio], [seen.tokens.primary, seen.tokens.primary]);
    assert.deepEqual(seen.checked, { border: seen.tokens.primary, background: seen.tokens["surface-3"], title: "500" });
    assert.deepEqual(seen.unchecked, { border: seen.tokens.border, background: seen.tokens.surface, title: "500" });
    assert.equal(seen.detail, "12px");
  });
}

test("the field, switch, checkbox and radio card rules name no accent token, no weight 600 and no literal font size", () => {
  const rules = style.split("\n").filter((line) => /^\.(?:field|helper|counter-text|input|select|textarea|switch|checkbox|radio-card)/.test(line));
  assert.ok(rules.length >= 18, `found ${rules.length} control rules`);
  assert.deepEqual(rules.filter((line) => /var\(--(?:accent|on-accent)|font-weight:\s*(?:500|600)|font-size:\s*[0-9]/.test(line)), []);
});
