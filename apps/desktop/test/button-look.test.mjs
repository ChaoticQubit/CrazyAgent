import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { bundle, withPage } from "./browser.mjs";

const fixture = await bundle(new URL("./fixtures/button-look.mjs", import.meta.url).pathname);
const style = readFileSync(new URL("../src/style.css", import.meta.url), "utf8");
const CLEAR = "rgba(0, 0, 0, 0)";

/**
 * Mounts the buttons in one theme and reads their look. Input: the theme.
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
  test(`${theme}: a secondary button draws a hairline --border on --surface with ink words at weight 500`, async () => {
    const seen = await looks(theme);
    assert.deepEqual(seen.secondary, { background: seen.tokens.surface, border: seen.tokens.border, color: seen.tokens.text, weight: "500", size: seen.secondary.size });
  });

  test(`${theme}: a primary button is solid ink, --primary with --on-primary words, and no vermilion`, async () => {
    const seen = await looks(theme);
    assert.deepEqual(seen.primary, { background: seen.tokens.primary, border: seen.tokens.primary, color: seen.tokens["on-primary"], weight: "500", size: seen.primary.size });
  });

  test(`${theme}: a quiet button is --text-2 words with no fill and no border`, async () => {
    const seen = await looks(theme);
    assert.deepEqual(seen.quiet, { background: CLEAR, border: CLEAR, color: seen.tokens["text-2"], weight: "500", size: seen.quiet.size });
  });

  test(`${theme}: a danger button keeps its state colors and a small button takes the 12 px secondary step`, async () => {
    const seen = await looks(theme);
    assert.deepEqual([seen.danger.background, seen.danger.color], [seen.tokens["danger-solid"], seen.tokens["on-danger"]]);
    assert.equal(seen.small.size, "12px");
    assert.equal(seen.small.border, seen.tokens.border);
  });
}

test("the .btn family names no accent token or literal size, and primary and quiet hover on neutral tokens", () => {
  const rules = style.split("\n").filter((line) => line.startsWith(".btn"));
  assert.deepEqual(rules.filter((line) => /var\(--(?:accent|on-accent)/.test(line)), []);
  assert.deepEqual(rules.filter((line) => /font-size:\s*[0-9]/.test(line)), []);
  assert.ok(rules.includes(".btn.primary:hover { background: var(--primary-hover); border-color: var(--primary-hover); }"), "primary hover");
  assert.ok(rules.includes(".btn.quiet:hover { background: var(--surface-2); }"), "quiet hover");
  assert.ok(rules.includes(".btn.small { height: 28px; padding: 0 10px; font-size: var(--font-secondary); }"), "small size token");
});
