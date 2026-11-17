import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { bundle, withPage } from "./browser.mjs";

const fixture = await bundle(new URL("./fixtures/agents-look.mjs", import.meta.url).pathname);
const agents = readFileSync(new URL("../src/screens/project/agents.tsx", import.meta.url), "utf8");
const CLEAR = "rgba(0, 0, 0, 0)";

/**
 * Draws the Agents tab's grouping in one theme and reads its look. Input: the theme.
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
  test(`${theme}: the Agents tab's lists are hairline rows with no box, closed by one hairline`, async () => {
    const seen = await looks(theme);
    const line = `1px ${seen.border}`;
    for (const name of ["team", "research"]) {
      assert.deepEqual(seen[name], { side: "0px", top: "0px", bottom: line, corner: "0px", fill: CLEAR, first: "0px", last: line }, name);
    }
    assert.equal(seen.mates.bottom.split(" ")[0], "0px", "a nested teammates list adds no second closing line");
  });

  test(`${theme}: the fold label, team and research names and task titles sit at 500`, async () => {
    const seen = await looks(theme);
    assert.deepEqual(seen.weights, { fold: "500", lead: "500", task: "500", job: "500" });
  });
}

test("both Agents tab views carry the pj-agents hook the hairline rows hang on", () => {
  assert.equal(agents.match(/return <div className="pj-stack pj-agents">/g)?.length, 2);
  assert.doesNotMatch(agents, /return <div className="pj-stack">/);
});
