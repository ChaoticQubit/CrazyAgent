import assert from "node:assert/strict";
import test from "node:test";
import { bundle, withPage } from "./browser.mjs";

const fixture = await bundle(new URL("./fixtures/recount-panel.mjs", import.meta.url).pathname);

/** Mounts the recount offer, then runs steps. Input: the steps, as source that returns an
 * object. Output: what the steps returned. */
function steps(source) {
  return withPage(async ({ evaluate }) => {
    await evaluate(fixture);
    return evaluate(`(async () => { const first = await Fixture.mount(); ${source} })()`);
  });
}

test("both recount choices wait while one is with the engine, and a refusal lets the owner choose again", { timeout: 60_000 }, async () => {
  const seen = await steps(`
    const asked = await Fixture.click("Use new numbers");
    const again = await Fixture.click("Keep old numbers");
    const refused = await Fixture.answer({ kind: "error", code: "usage_outcome_unknown" });
    return { first, asked, again, refused };
  `);
  assert.deepEqual(seen.first.off, [false, false]);
  assert.equal(seen.asked.missed, null);
  assert.deepEqual(seen.asked.off, [true, true], "both choices wait for the engine");
  assert.deepEqual(seen.again.choices, [true], "a second click sends no second choice");
  assert.equal(seen.refused.done, 0);
  assert.match(seen.refused.alert ?? "", /couldn't confirm/u);
  assert.deepEqual(seen.refused.off, [false, false], "a refusal lets the owner choose again");
});

test("a choice the engine applied keeps both off while the page reads again", { timeout: 60_000 }, async () => {
  const seen = await steps(`
    await Fixture.click("Keep old numbers");
    const applied = await Fixture.answer({ kind: "ok", value: undefined });
    const again = await Fixture.click("Use new numbers");
    return { applied, again };
  `);
  assert.equal(seen.applied.done, 1, "the page reads again once");
  assert.deepEqual(seen.applied.off, [true, true]);
  assert.deepEqual(seen.again.choices, [false], "no second choice reaches the engine");
});
