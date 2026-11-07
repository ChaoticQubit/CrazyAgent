import assert from "node:assert/strict";
import test from "node:test";
import { readFileSync } from "node:fs";
import { bundle, withPage } from "./browser.mjs";

const fixture = await bundle(new URL("./fixtures/overview-activity.mjs", import.meta.url).pathname);

const item = (id, kind, goal, text, actor = "Engine") => ({ id, at_ms: Date.UTC(2026, 10, 7, 7, id), kind, actor, goal, text, change_set: null, undone: false });
const ITEMS = [
  item(1, "created", null, "You created Harbor Notes"),
  item(2, "research_saved", null, "Market research saved"),
  item(3, "accepted", "Build the service", "You accepted Ada's login"),
  item(4, "lead_merged", "Build the service", "Master merged change set #4"),
  item(5, "test", "Write the docs", "Docs check: 12 passed", "Ada (@docs)"),
  item(6, "phase", "Write the docs", "Building: guides next", "Ada (@docs)"),
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
    { label: "Write the docs", open: "true", rows: ["Ada (@docs) noted: Building: guides next", "Ada (@docs) noted: Docs check: 12 passed"] },
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
  assert.deepEqual(seen.tests.groups, [{ label: "Write the docs", open: "true", rows: ["Ada (@docs) noted: Docs check: 12 passed"] }]);
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
    { text: "Ada (@docs) noted: Building: guides next", goal: "Write the docs" },
    { text: "Ada (@docs) noted: Docs check: 12 passed", goal: "Write the docs" },
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

test("rows a later read brings in are marked fresh, and the first read marks none", { timeout: 60_000 }, async () => {
  const older = { kind: "ok", value: { project_id: "alpha", items: ITEMS.slice(0, 6), more: false } };
  const seen = await withPage(async ({ evaluate }) => {
    await evaluate(fixture);
    return evaluate(`(async () => {
      await Fixture.mount("alpha");
      const first = await Fixture.reply(${JSON.stringify(older)});
      await Fixture.wait(450);
      const later = await Fixture.reply(${JSON.stringify(ok)});
      return { first, later };
    })()`);
  });
  assert.deepEqual(seen.first.fresh, []);
  assert.deepEqual(seen.later.fresh, ["Ada finished Login"]);
});

test("a project switch puts the chips back on All and keeps the view", { timeout: 60_000 }, async () => {
  const seen = await steps(`
    const timeline = await Fixture.click("[role=radio]", "Timeline");
    const merges = await Fixture.click("button[aria-pressed]", "Merges");
    await Fixture.move("beta");
    let moved = await Fixture.reply(${JSON.stringify(ok)});
    if (moved.waiting > 0) moved = await Fixture.reply(${JSON.stringify(ok)});
    return { timeline, merges, moved };
  `);
  assert.equal(seen.timeline.missed, null);
  assert.deepEqual(seen.merges.pressed, ["Merges"]);
  assert.deepEqual(seen.moved.pressed, ["All"]);
  assert.deepEqual(seen.moved.view, ["Timeline"]);
});

const APP_CSS = [
  readFileSync(new URL("../src/ui/tokens.css", import.meta.url), "utf8"),
  readFileSync(new URL("../src/style.css", import.meta.url), "utf8").replace(/@import[^;]*;/g, ""),
  readFileSync(new URL("../src/screens/project/projects.css", import.meta.url), "utf8"),
].join("\n");
const LONG_GOAL = "Building V2: the booking flow with calendar sync, payments through one adapter and the email receipts for every plan";
const STEPS = ["Plan: cases listed", "Architecture done: payments go through one adapter", "Building: calendar endpoints are in", "Testing: checkout end to end", "Review: findings fixed", "Docs: guides for the booking API", "Release: notes drafted"];
const PATHED = { kind: "ok", value: { project_id: "alpha", more: false, items: [
  ...STEPS.map((text, index) => item(10 + index, "phase", LONG_GOAL, text)),
  item(20, "task_finished", LONG_GOAL, "Juniper finished Calendar API"),
  item(9, "phase", "Write the docs", "Writing: guides next"),
] } };

test("in the real Overview column of a 1024 px window a long goal keeps a readable name and its phase path stays inside the panel", { timeout: 60_000 }, async () => {
  const seen = await withPage(async ({ evaluate, send }) => {
    await send("Emulation.setDeviceMetricsOverride", { width: 1024, height: 768, deviceScaleFactor: 1, mobile: false });
    await evaluate(fixture);
    return evaluate(`(async () => {
      await Fixture.mount("alpha");
      Fixture.inOverview(${JSON.stringify(APP_CSS)});
      const shown = await Fixture.reply(${JSON.stringify(PATHED)});
      return { shown, measured: Fixture.measure() };
    })()`);
  });
  assert.ok(seen.measured.column > 380 && seen.measured.column < 440, `the section sits in the real Overview column, ${seen.measured.column} px`);
  assert.deepEqual(seen.shown.groups.map((group) => group.label), [LONG_GOAL, "Write the docs"]);
  assert.equal(seen.measured.names.length, 2);
  for (const width of seen.measured.names) assert.ok(width >= 200, `a goal name is at least 200 px wide, not ${width} px`);
  assert.deepEqual(seen.measured.past, [], "no part of a goal header runs past its panel");
  assert.ok(seen.measured.sideways <= 0, "the page does not scroll sideways");
  assert.deepEqual(seen.shown.paths[0], ["+3", "✓ Testing", "✓ Review", "✓ Docs", "Release"], "the path shows the last four steps and counts the earlier ones");
});

test("a chip keeps each goal's phase path and its (no goal) label, which come from the whole read", { timeout: 60_000 }, async () => {
  const read = { kind: "ok", value: { project_id: "alpha", more: false, items: [
    item(1, "created", null, "You created Harbor Notes"),
    item(2, "research_saved", null, "Market research saved"),
    item(3, "test", "Research", "Sources check: 3 passed"),
    item(4, "phase", "Build the service", "Building: login next"),
    item(5, "task_finished", "Build the service", "Ada finished Login"),
  ] } };
  const seen = await withPage(async ({ evaluate }) => {
    await evaluate(fixture);
    return evaluate(`(async () => {
      await Fixture.mount("alpha");
      const all = await Fixture.reply(${JSON.stringify(read)});
      const tasks = await Fixture.click("button[aria-pressed]", "Tasks");
      return { all, tasks };
    })()`);
  });
  assert.deepEqual(seen.all.groups.map((group) => group.label), ["Build the service", "Research", "Research (no goal)", "Project"]);
  assert.deepEqual(seen.all.paths[0], ["Building"]);
  assert.equal(seen.tasks.missed, null);
  assert.deepEqual(seen.tasks.groups.map((group) => group.label), ["Build the service", "Research (no goal)"], "the no-goal group keeps its label while the chip hides the named goal");
  assert.deepEqual(seen.tasks.paths[0], ["Building"], "the path stays while the chip hides the phase rows");
});
