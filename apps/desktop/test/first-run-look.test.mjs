import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { bundle, withPage } from "./browser.mjs";

const fixture = await bundle(new URL("./fixtures/first-run-look.mjs", import.meta.url).pathname);
const intake = readFileSync(new URL("../src/screens/intake/intake.css", import.meta.url), "utf8");
const CLEAR = "rgba(0, 0, 0, 0)";

/**
 * Mounts the first run on one step in one theme and reads its look. Inputs: the theme and the
 * step. Output: what `Fixture.look()` returns.
 */
function looks(theme, step) {
  return withPage(async (page) => {
    await page.evaluate(fixture);
    await page.evaluate(`Fixture.mount(${JSON.stringify(theme)}, ${JSON.stringify(step)})`);
    return page.evaluate("Fixture.look()");
  });
}

for (const theme of ["light", "dark"]) {
  test(`${theme}: the current setup step is a --surface chip on a hairline at 500, the others muted words`, async () => {
    const seen = await looks(theme, "profile");
    const t = seen.tokens;
    assert.deepEqual(seen.current, { fill: t.surface, edge: `1px ${t.border}`, corner: "999px", color: t.text, weight: "500", size: "12px", line: "16px" });
    const { edge, ...other } = seen.other;
    assert.ok(edge.startsWith("0px "), edge);
    assert.deepEqual(other, { fill: CLEAR, corner: "0px", color: t["text-3"], weight: "400", size: "12px", line: "16px" });
  });

  for (const [step, words] of [["profile", "Save and continue"], ["accounts", "Add account"]]) {
    test(`${theme}: on the ${step} step, ${words} is the one filled button, in ink`, async () => {
      const seen = await looks(theme, step);
      assert.deepEqual(seen.filled, [{ words, fill: seen.tokens.primary, color: seen.tokens["on-primary"] }]);
    });
  }
}

test("the setup step rules in intake.css name no weight 600 or 700 and no literal size", () => {
  const rules = intake.split("\n").filter((line) => line.startsWith(".in-steps"));
  assert.equal(rules.length, 2);
  assert.deepEqual(rules.filter((line) => /font-weight:\s*[67]00|font-size:\s*[0-9]/.test(line)), []);
});
