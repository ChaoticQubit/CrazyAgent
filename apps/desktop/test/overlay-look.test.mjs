import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { bundle, withPage } from "./browser.mjs";

const fixture = await bundle(new URL("./fixtures/overlay-look.mjs", import.meta.url).pathname);
const style = readFileSync(new URL("../src/style.css", import.meta.url), "utf8");

/**
 * Opens one overlay in one theme and reads its look. Inputs: "dialog" or "drawer", and the
 * theme. Output: what `Fixture.look()` returns.
 */
function looks(kind, theme) {
  return withPage(async (page) => {
    await page.evaluate(fixture);
    await page.evaluate(`Fixture.mount(${JSON.stringify(kind)}, ${JSON.stringify(theme)})`);
    await page.evaluate("Fixture.frames()");
    return page.evaluate("Fixture.look()");
  });
}

for (const theme of ["light", "dark"]) {
  test(`${theme}: a dialog is --surface with --text, a hairline --border, the 12 px dialog radius, --elev-2 and a --scrim backdrop`, async () => {
    const seen = await looks("dialog", theme);
    assert.equal(seen.open, true);
    assert.deepEqual([seen.background, seen.color, seen.edges.top, seen.radius], [seen.tokens.surface, seen.tokens.text, seen.tokens.border, "12px"]);
    assert.deepEqual([seen.shadow, seen.backdrop], [seen.tokens.shadow, seen.tokens.scrim]);
    assert.deepEqual([seen.head, seen.foot], [seen.tokens.border, seen.tokens.border]);
  });

  test(`${theme}: a drawer is --surface with a hairline --border on its left edge only, --elev-2 and a --scrim backdrop`, async () => {
    const seen = await looks("drawer", theme);
    assert.equal(seen.open, true);
    assert.deepEqual([seen.background, seen.edges.left, seen.edges.leftWidth], [seen.tokens.surface, seen.tokens.border, "1px"]);
    assert.deepEqual([seen.shadow, seen.backdrop], [seen.tokens.shadow, seen.tokens.scrim]);
  });

  for (const kind of ["dialog", "drawer"]) {
    test(`${theme}: a ${kind} title takes the 16/24 subtitle step at weight 600`, async () => {
      const seen = await looks(kind, theme);
      assert.deepEqual(seen.title, { size: "16px", line: "24px", weight: "600" });
    });
  }
}

test("the scrim, dialog, drawer and menu rules name no accent token and no literal font size", () => {
  const rules = style.split("\n").filter((line) => /^(?:\.scrim|dialog\.|\.dialog-|\.menu)/.test(line));
  assert.ok(rules.length >= 15, `found ${rules.length} overlay rules`);
  assert.deepEqual(rules.filter((line) => /var\(--(?:accent|on-accent)|font-size:\s*[0-9]/.test(line)), []);
  assert.ok(rules.includes(".dialog-head h2 { flex: 1; min-width: 0; font-size: var(--font-subtitle); line-height: var(--leading-subtitle); }"));
});
