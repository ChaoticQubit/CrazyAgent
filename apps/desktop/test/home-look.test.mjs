import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { bundle, withPage } from "./browser.mjs";

const fixture = await bundle(new URL("./fixtures/home-look.mjs", import.meta.url).pathname);
const style = readFileSync(new URL("../src/style.css", import.meta.url), "utf8");

/**
 * Mounts Home in one theme and reads its look. Input: the theme.
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
  test(`${theme}: Home names projects and setup steps at 500, and its card lines sit on the type scale`, async () => {
    const seen = await looks(theme);
    assert.deepEqual([seen.cards, seen.need.count, seen.setup.items > 0], [1, 1, true], JSON.stringify(seen));
    assert.equal(seen.name.weight, "500");
    assert.deepEqual([seen.activity.size, seen.activity.line], ["13px", "20px"]);
    assert.deepEqual([seen.meta.size, seen.meta.line], ["12px", "16px"]);
    assert.equal(seen.need.project, "500");
    assert.equal(seen.setup.title, "500");
  });
}

test("the Home rules name no accent token, no weight 600 or 700, no literal font size and no 3:1 input edge on a card", () => {
  const rules = style.split("\n").filter((line) => /^\.(?:home|cards|project-card|setup-panel|needs?\b)/.test(line));
  assert.ok(rules.length >= 18, `found ${rules.length} Home rules`);
  assert.deepEqual(rules.filter((line) => /var\(--(?:accent|on-accent)|font-weight:\s*[67]00|font-size:\s*[0-9]/.test(line)), []);
  assert.deepEqual(rules.filter((line) => /var\(--border-input\)/.test(line)), []);
  assert.ok(rules.includes(".project-card:hover { box-shadow: var(--elev-1); }"), "a card lifts on hover with its hairline kept");
});
