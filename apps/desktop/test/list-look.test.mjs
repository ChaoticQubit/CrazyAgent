import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { bundle, withPage } from "./browser.mjs";

const fixture = await bundle(new URL("./fixtures/list-look.mjs", import.meta.url).pathname);
const sheet = (path) => readFileSync(new URL(path, import.meta.url), "utf8");
const style = sheet("../src/style.css");
const changes = sheet("../src/screens/changes/changes.css");
const settings = sheet("../src/screens/settings/settings.css");
const CLEAR = "rgba(0, 0, 0, 0)";

/**
 * Mounts the list rows in one theme and reads their look. Input: the theme.
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
  test(`${theme}: an account table has a 12 px --text-3 column head, clear rows on hairline dividers and --primary pick boxes`, async () => {
    const seen = await looks(theme);
    assert.deepEqual(seen.head, { size: "12px", color: seen.tokens["text-3"] });
    assert.deepEqual(seen.account, { background: CLEAR, divider: seen.tokens.border });
    assert.equal(seen.pick, seen.tokens.primary);
    assert.equal(seen.item, "500");
  });

  test(`${theme}: the change row shown is --surface-3 with an ink --primary edge, the others clear, and every row title sits at 500`, async () => {
    const seen = await looks(theme);
    assert.deepEqual(seen.change.shown, { background: seen.tokens["surface-3"], divider: seen.tokens.border, edge: `${seen.tokens.primary} 3px 0px 0px 0px inset`, title: "500" });
    assert.deepEqual(seen.change.other, { background: CLEAR, divider: seen.tokens.border, edge: "none", title: "500" });
  });
}

test("the list row and table rules name no accent token, no literal weight and no literal font size, and focus and hover use neutral tokens", () => {
  const rules = [style, changes, settings].flatMap((text) => text.split("\n")).filter((line) => /^\.(?:windowed|list-head|accounts?-|item-row|ch-row|pool-add)/.test(line));
  assert.ok(rules.length >= 20, `found ${rules.length} list rules`);
  assert.deepEqual(rules.filter((line) => /var\(--(?:accent|on-accent|surface-raised)|font-weight:\s*[0-9]|font-size:\s*[0-9]/.test(line)), []);
  assert.ok(rules.includes(".ch-row:focus-visible { outline: 2px solid var(--focus); outline-offset: -2px; }"), "change row focus ring");
  assert.ok(rules.includes(".account-row:hover { background: var(--surface-2); }"), "account row hover");
});
