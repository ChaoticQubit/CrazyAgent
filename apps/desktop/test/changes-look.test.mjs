import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { bundle, withPage } from "./browser.mjs";

const fixture = await bundle(new URL("./fixtures/changes-look.mjs", import.meta.url).pathname);
const changes = readFileSync(new URL("../src/screens/changes/changes.css", import.meta.url), "utf8");
const CLEAR = "rgba(0, 0, 0, 0)";

/**
 * Mounts the change set detail in one theme and reads its look once the diff shows.
 * Input: the theme. Output: what `Fixture.look()` returns.
 */
function looks(theme) {
  return withPage(async (page) => {
    await page.evaluate(fixture);
    await page.evaluate(`Fixture.mount(${JSON.stringify(theme)})`);
    await page.evaluate("Fixture.settled()");
    return page.evaluate("Fixture.look()");
  });
}

for (const theme of ["light", "dark"]) {
  test(`${theme}: the change set detail titles at 500, Accept is solid ink and Discard stays a red text button`, async () => {
    const seen = await looks(theme);
    assert.equal(seen.title, "500");
    assert.deepEqual(seen.accept, { background: seen.tokens.primary, border: seen.tokens.primary, color: seen.tokens["on-primary"] });
    assert.deepEqual(seen.discard, { background: CLEAR, border: CLEAR, color: seen.tokens.danger });
  });

  test(`${theme}: a file mark is a --text-2 pill on a hairline, and the diff is 12/18 mono at 400`, async () => {
    const seen = await looks(theme);
    assert.deepEqual(seen.mark, { color: seen.tokens["text-2"], border: seen.tokens.border });
    assert.deepEqual(seen.diff, { size: "12px", line: "18px", weight: "400", mono: true });
  });
}

test("changes.css names no accent token, no undefined token, no literal radius and no literal font size", () => {
  const rules = changes.split("\n").filter((line) => line.startsWith(".ch-") || line.startsWith(".pj-rows.ch-list"));
  assert.ok(rules.length >= 14, `found ${rules.length} Changes rules`);
  assert.deepEqual(rules.filter((line) => /var\(--(?:accent|on-accent|surface-raised|text-secondary)|999px|font-size:\s*[0-9]|font:[^;]*\b[0-9.]+px/.test(line)), []);
  assert.ok(rules.includes(".ch-mark { padding: 0 6px; border: 1px solid var(--border); border-radius: var(--radius-pill); color: var(--text-2); }"), "file mark");
});
