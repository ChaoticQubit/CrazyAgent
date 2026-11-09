import "./load-source.mjs";
import assert from "node:assert/strict";
import test from "node:test";
import { bundle, withPage } from "./browser.mjs";

const fixture = await bundle(new URL("./fixtures/team-settings-layout.mjs", import.meta.url).pathname);

const harness = (name, role) => ({ harness: name, role, agents_max: 2, tools: true, live: 0, delegation: null, delegation_label: null, runnable: true });
const STATUS = {
  project_id: "alpha", keep_warm: false, idle_stop_secs: 600, paused: false, min_agents: 1, max_agents: 3, settings_version: 1, master_lands: "merges",
  harnesses: [harness("codex", "master"), harness("codex", "sub"), harness("claude", "sub")], agents: [], tasks: [], messages: [], plan: [],
  events: { recent: [], waiting: [], notifications: [], paused: false, questions: [],
    wake: { app: null, app_version: 0, project: null, project_version: 0, agents: [], cleared: [], effective: { question: true, finished: true, failed: true, change_set_ready: true, progress: false, hourly_cap: 30 }, level: "shipped" } },
};
const APPLIED = { outcome: "applied", pending: false, rejection: null };
const REFUSED = { outcome: "rejected", pending: false, rejection: "engine_refused" };

const ROWS = [
  "On master harness switch", "Spread harnesses across accounts", "Max harnesses per project", "Max teammates per harness",
  "Under Full access, merging into main", "Ask me before merging into the master's branch", "Agent branches",
  "Sub-agent work lands on its lead's branch", "Refuse a change set when one new folder adds more than",
  "Workflow nudges", "Task-start reminder", "Re-ask a sub-agent's empty reply",
  "Claude Code sub-agents", "Sub-agent depth", "Sub-agent questions go to", "Overlapping edits", "Terminals per window", "Catch up with main", "Finished agents",
  "Messages to Claude Code agents", "Claude Code terminal display", "Messages to Codex agents", "Messages to rcli agents",
  "Each task may run", "Each agent session may run", "Stop an idle sub-agent after", "Background commands at task end", "Token budget",
];

/** Opens Agent settings over the fake engine with every team save given one answer, and runs
 * the steps. Inputs: the answer and the steps, which get the page's handles. Output: what the
 * steps return. */
function onSettings(answer, steps) {
  return withPage(async (page) => {
    await page.send("Emulation.setDeviceMetricsOverride", { width: 1440, height: 900, deviceScaleFactor: 1, mobile: false });
    await page.evaluate(fixture);
    assert.equal(await page.evaluate(`Fixture.mount("", "page", ${JSON.stringify(STATUS)})`), true, "Agent settings mount");
    await page.evaluate(`Fixture.answerSaves(${JSON.stringify(answer)})`);
    return steps(page);
  });
}

test("every Agent setting keeps the owner's pick on screen while the team read after its save is on its way", { timeout: 120_000 }, async () => {
  const seen = await onSettings(APPLIED, async (page) => {
    const rows = {};
    for (const row of ROWS) rows[row] = await page.evaluate(`Fixture.pickOther(${JSON.stringify(row)})`);
    return rows;
  });
  const missing = ROWS.filter((row) => seen[row] === null);
  assert.deepEqual(missing, [], "each row offers another value");
  const reverted = ROWS.filter((row) => seen[row].shown !== seen[row].picked).map((row) => `${row}: picked ${seen[row].picked}, shows ${seen[row].shown}`);
  assert.deepEqual(reverted, [], "no row falls back to the value its save replaced");
});

test("a saved branch name stays in its box while the team read after its save is on its way", { timeout: 120_000 }, async () => {
  const seen = await onSettings(APPLIED, async (page) => ({
    master: await page.evaluate(`Fixture.saveText("tg-master-branch-alpha", "release")`),
    land: await page.evaluate(`Fixture.saveText("tg-land-on-alpha", "trunk")`),
  }));
  assert.deepEqual(seen, { master: "release", land: "trunk" });
});

test("the old value of a select can be picked again right after a save, and that pick reaches the engine", { timeout: 120_000 }, async () => {
  const seen = await onSettings(APPLIED, async (page) => {
    const first = await page.evaluate(`Fixture.pickOther("Max harnesses per project")`);
    const shownAfterSave = await page.evaluate(`Fixture.selected("tg-max-harnesses-alpha")`);
    const back = await page.evaluate(`Fixture.pickOther("Max harnesses per project", ${JSON.stringify(first.before)})`);
    return { first, shownAfterSave, back, sent: await page.evaluate(`Fixture.sentTo("desktop_team_max_harnesses")`) };
  });
  assert.equal(seen.shownAfterSave, seen.first.picked, "the select holds the new value, so picking the old one is a real change");
  assert.equal(seen.back.before, seen.first.picked);
  assert.equal(seen.back.picked, seen.first.before, "the second pick is the old value");
  assert.equal(seen.sent.args.length, 2, "both picks are saved");
  assert.equal(seen.back.shown, seen.back.picked);
});

test("a refused save shows the saved value again, on a select and on a segmented row", { timeout: 120_000 }, async () => {
  const seen = await onSettings(REFUSED, async (page) => ({
    select: await page.evaluate(`Fixture.pickOther("Token budget")`),
    segmented: await page.evaluate(`Fixture.pickOther("Spread harnesses across accounts")`),
    limit: await page.evaluate(`Fixture.pickOther("Each task may run")`),
  }));
  for (const [row, value] of Object.entries(seen)) assert.equal(value.shown, value.before, `${row} goes back to the saved value`);
});

test("a held pick lets go once a read moves the value, so a later read back to the old value shows", { timeout: 120_000 }, async () => {
  const seen = await onSettings(APPLIED, async (page) => {
    const first = await page.evaluate(`Fixture.pickOther("Each task may run")`);
    await page.evaluate(`Fixture.reread(${JSON.stringify({ ...STATUS, turn_limit_secs: Number(first.picked), settings_version: 2 })})`);
    const moved = await page.evaluate(`Fixture.selected("tg-turn-limit-alpha")`);
    await page.evaluate(`Fixture.reread(${JSON.stringify({ ...STATUS, settings_version: 3 })})`);
    return { first, moved, back: await page.evaluate(`Fixture.selected("tg-turn-limit-alpha")`) };
  });
  assert.equal(seen.moved, seen.first.picked, "the read that holds the pick shows it");
  assert.equal(seen.back, seen.first.before, "another window's change back to the old value shows, not the earlier pick");
});
