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
  test(`${theme}: the Agents tab's Team table and lists are hairline rows with no box, closed by one hairline`, async () => {
    const seen = await looks(theme);
    const line = `1px ${seen.border}`;
    assert.deepEqual(seen.team, { side: "0px", top: "0px", bottom: line, corner: "0px", fill: CLEAR }, "the boxed Team table");
    for (const name of ["tasks", "research"]) {
      assert.deepEqual(seen[name], { side: "0px", top: "0px", bottom: line, corner: "0px", fill: CLEAR, first: "0px", last: line }, name);
    }
    assert.deepEqual([seen.mates.side, seen.mates.bottom.split(" ")[0], seen.mates.fill], ["0px", "0px", CLEAR], "a teammates list inside a name cell adds no box and no closing line");
  });

  test(`${theme}: the fold label, team names and their cells, task titles, the Research heading and research names sit at 500`, async () => {
    const seen = await looks(theme);
    assert.deepEqual(seen.weights, { fold: "500", lead: "500", cell: "500", task: "500", heading: "500", job: "500" });
  });
}

test("both Agents tab views carry the pj-agents hook the hairline rows hang on", () => {
  assert.equal(agents.match(/return <div className="pj-stack pj-agents">/g)?.length, 2);
  assert.doesNotMatch(agents, /return <div className="pj-stack">/);
});
