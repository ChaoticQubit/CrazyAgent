import assert from "node:assert/strict";
import test from "node:test";
import "./load-source.mjs";
import { bundle, withPage } from "./browser.mjs";

const coord = await import("../src/screens/team/coordination.tsx");
const harnessPanel = await bundle(new URL("./fixtures/terminal-harnesses.mjs", import.meta.url).pathname);

test("DEC-187: a switch back shows the kept terminal with the output it got while hidden and attaches nothing, and an ended one attaches again on show", async (t) => {
  const harness = coord.COORD_HARNESSES[0];
  const seen = await withPage(async ({ evaluate }) => {
    await evaluate(harnessPanel);
    await evaluate(`Fixture.open("master", ${JSON.stringify(harness)})`);
    assert.equal(await evaluate("Fixture.until(() => Fixture.view().live)"), true, "the terminal went live");
    await evaluate('Fixture.output("before the switch\\r\\n")');
    assert.equal(await evaluate('Fixture.until(() => Fixture.view().text.includes("before the switch"))'), true);
    await evaluate("Fixture.away()");
    const parked = await evaluate('({ panel: document.querySelector(".tg-terminal") !== null, host: document.querySelector(".tg-terminal-parking > .tg-terminal-host") !== null, focus: Boolean(document.activeElement?.closest(".xterm")) })');
    await evaluate('Fixture.output("while hidden\\r\\n")');
    const showMs = await evaluate(`Fixture.back("master", ${JSON.stringify(harness)})`);
    const shown = await evaluate('Fixture.until(() => Fixture.view().text.includes("before the switch") && Fixture.view().text.includes("while hidden"))');
    const live = await evaluate("Fixture.view().live");
    const attachesAfterSwitch = await evaluate("Fixture.attaches()");
    await evaluate("Fixture.close()");
    assert.equal(await evaluate('Fixture.until(() => document.querySelector(".tg-terminal-state[role=alert]") !== null)'), true, "the closed terminal says it ended");
    await evaluate("Fixture.away()");
    await evaluate(`Fixture.back("master", ${JSON.stringify(harness)})`);
    const again = await evaluate("Fixture.until(() => Fixture.view().live)");
    return { parked, showMs, shown, live, attachesAfterSwitch, again, attachesAfterEnded: await evaluate("Fixture.attaches()") };
  });
  t.diagnostic(`Chrome mount of a kept terminal, render and show: ${seen.showMs.toFixed(2)} ms`);
  assert.deepEqual(seen.parked, { panel: false, host: true, focus: false }, "away keeps the view parked, unfocused");
  assert.equal(seen.shown, true, "the switch back shows the kept scrollback and the output that came while hidden");
  assert.equal(seen.live, true);
  assert.equal(seen.attachesAfterSwitch, 1, "a switch back attaches nothing");
  assert.equal(seen.again, true, "an ended terminal shown again is live again");
  assert.equal(seen.attachesAfterEnded, 2, "an ended terminal attaches once more when shown again");
});
