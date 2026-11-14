import "./load-source.mjs";
import assert from "node:assert/strict";
import test from "node:test";
import { bundle, withPage } from "./browser.mjs";

const { sameOrNext } = await import("../src/app.tsx");
const watchPage = await bundle(new URL("./fixtures/team-alerts-renders.mjs", import.meta.url).pathname);

const LOOKS = 6;
const reads = (asked) => asked.filter((command) => command === "desktop_team_events" || command === "desktop_question_counts").length;

test("a count read the same as the one shown keeps the shown map, and any change gives the new one", () => {
  const shown = new Map([["alpha", 2], ["beta", 1]]);
  assert.equal(sameOrNext(shown, new Map([["beta", 1], ["alpha", 2]])), shown);
  const empty = new Map();
  assert.equal(sameOrNext(empty, new Map()), empty);
  for (const next of [new Map([["alpha", 3], ["beta", 1]]), new Map([["alpha", 2]]), new Map([["alpha", 2], ["beta", 1], ["gamma", 1]]), new Map([["alpha", 2], ["gamma", 1]])]) {
    assert.equal(sameOrNext(shown, next), next, JSON.stringify([...next]));
  }
});

test("the root team alert look renders the app's tree only when the counts changed, at two engine reads a look", { timeout: 60_000 }, async () => {
  const seen = await withPage(async ({ evaluate }) => {
    await evaluate(watchPage);
    const started = await evaluate(`Fixture.start({ alpha: [2, 1], beta: [0, 3] })`);
    const same = await evaluate(`Fixture.looks({ alpha: [2, 1], beta: [0, 3] }, ${LOOKS})`);
    const changed = await evaluate(`Fixture.looks({ alpha: [3, 1], beta: [0, 3] }, 1)`);
    const settled = await evaluate(`Fixture.looks({ alpha: [3, 1], beta: [0, 3] }, ${LOOKS})`);
    const idle = await evaluate(`Fixture.looks({ alpha: [3, 0], beta: [0, 0] }, 1)`);
    return { started, same, changed, settled, idle };
  });
  assert.equal(seen.started.armed, true, "the next look is armed");
  assert.equal(seen.same.renders, seen.started.renders, `${LOOKS} looks that read the same counts render nothing`);
  assert.equal(seen.changed.renders, seen.same.renders + 1, "a changed count renders once");
  assert.equal(seen.settled.renders, seen.changed.renders, "the same counts again render nothing");
  assert.equal(seen.idle.renders, seen.settled.renders + 1, "agents stopping render once");
  assert.equal(reads(seen.same.asked) - reads(seen.started.asked), 2 * LOOKS, "each look reads the events and the question counts once");
});
