import assert from "node:assert/strict";
import test from "node:test";
import { bundle, withPage } from "./browser.mjs";

const fixture = await bundle(new URL("./fixtures/overview-activity.mjs", import.meta.url).pathname);

const item = (id, kind, goal, text) => ({ id, at_ms: Date.UTC(2026, 10, 7, 7, id), kind, actor: "Engine", goal, text, change_set: null, undone: false });
const ITEMS = [
  item(1, "created", null, "You created Harbor Notes"),
  item(2, "research_saved", null, "Market research saved"),
  item(3, "accepted", "Build the service", "You accepted Ada's login"),
  item(4, "lead_merged", "Build the service", "Master merged change set #4"),
  item(5, "test", "Write the docs", "Docs check: 12 passed"),
  item(6, "phase", "Write the docs", "Building: guides next"),
  item(7, "task_finished", "Build the service", "Ada finished Login"),
];
const ok = { kind: "ok", value: { project_id: "alpha", items: ITEMS, more: false } };

/** Mounts Recent activity with the seven events above, then runs steps. Input: the steps, as
 * source that returns an object. Output: what the steps returned. */
function steps(source) {
  return withPage(async ({ evaluate }) => {
    await evaluate(fixture);
    return evaluate(`(async () => { await Fixture.mount("alpha"); const first = await Fixture.reply(${JSON.stringify(ok)}); ${source} })()`);
  });
}

test("By goal opens every goal but Research, and a goal's header closes and opens it", { timeout: 60_000 }, async () => {
  const seen = await steps(`
    const research = await Fixture.click(".pj-activity-goal[aria-label='Research'] [aria-expanded]", "Research");
    const closed = await Fixture.click(".pj-activity-goal[aria-label='Build the service'] [aria-expanded]", "Build the service");
    return { first, research, closed };
  `);
  assert.deepEqual(seen.first.view, ["By goal"]);
  assert.deepEqual(seen.first.groups, [
    { label: "Build the service", open: "true", rows: ["Ada finished Login", "Master merged change set #4", "You accepted Ada's login"] },
    { label: "Write the docs", open: "true", rows: ["Building: guides next", "Docs check: 12 passed"] },
    { label: "Research", open: "false", rows: [] },
    { label: "Project", open: "true", rows: ["You created Harbor Notes"] },
  ]);
  assert.equal(seen.research.missed, null);
  assert.deepEqual(seen.research.groups[2], { label: "Research", open: "true", rows: ["Market research saved"] });
  assert.deepEqual(seen.closed.groups[0], { label: "Build the service", open: "false", rows: [] });
});

test("a filter chip keeps only its kind of event and hides goals left empty", { timeout: 60_000 }, async () => {
  const seen = await steps(`
    const merges = await Fixture.click("button[aria-pressed]", "Merges");
    const tests = await Fixture.click("button[aria-pressed]", "Tests");
    const all = await Fixture.click("button[aria-pressed]", "All");
    return { first, merges, tests, all };
  `);
  assert.deepEqual(seen.first.pressed, ["All"]);
  assert.equal(seen.merges.missed, null);
  assert.deepEqual(seen.merges.pressed, ["Merges"]);
  assert.deepEqual(seen.merges.groups, [{ label: "Build the service", open: "true", rows: ["Master merged change set #4"] }]);
  assert.deepEqual(seen.tests.groups, [{ label: "Write the docs", open: "true", rows: ["Docs check: 12 passed"] }]);
  assert.deepEqual(seen.all.groups.map((group) => group.label), ["Build the service", "Write the docs", "Research", "Project"]);
});

test("Timeline lists every event newest first with its goal, and the chips filter it too", { timeout: 60_000 }, async () => {
  const seen = await steps(`
    const timeline = await Fixture.click("[role=radio]", "Timeline");
    const tasks = await Fixture.click("button[aria-pressed]", "Tasks");
    return { timeline, tasks };
  `);
  assert.equal(seen.timeline.missed, null);
  assert.deepEqual(seen.timeline.view, ["Timeline"]);
  assert.deepEqual(seen.timeline.groups, [], "Timeline has no goal groups");
  assert.deepEqual(seen.timeline.timeline, [
    { text: "Ada finished Login", goal: "Build the service" },
    { text: "Building: guides next", goal: "Write the docs" },
    { text: "Docs check: 12 passed", goal: "Write the docs" },
    { text: "Master merged change set #4", goal: "Build the service" },
    { text: "You accepted Ada's login", goal: "Build the service" },
    { text: "Market research saved", goal: "Research" },
    { text: "You created Harbor Notes", goal: "Project" },
  ]);
  assert.deepEqual(seen.tasks.timeline, [
    { text: "Ada finished Login", goal: "Build the service" },
    { text: "Market research saved", goal: "Research" },
  ]);
});
