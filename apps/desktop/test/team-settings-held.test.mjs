import "./load-source.mjs";
import assert from "node:assert/strict";
import test from "node:test";
import { bundle, withPage } from "./browser.mjs";

const fixture = await bundle(new URL("./fixtures/team-settings-layout.mjs", import.meta.url).pathname);
const appFixture = await bundle(new URL("./fixtures/app-settings-held.mjs", import.meta.url).pathname);

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
  "Claude Code sub-agents", "Sub-agent depth", "Sub-agent questions go to", "Overlapping edits", "Terminals per window", "Catch up with main", "Finished agents", "Terminal display",
  "Messages to Claude Code agents", "Messages to Codex agents",
  "Each task may run", "Each agent session may run", "Stop an idle sub-agent after", "Background commands at task end", "Token budget", "Master run time", "Sub-agent rounds per message",
];

/** Opens Agent settings or the master's settings over the fake engine with every team save given
 * one answer, and runs the steps. Inputs: the answer, the steps, which get the page's handles,
 * and "page" or "drawer". Output: what the steps return. */
function onSettings(answer, steps, view = "page") {
  return withPage(async (page) => {
    await page.send("Emulation.setDeviceMetricsOverride", { width: 1440, height: 900, deviceScaleFactor: 1, mobile: false });
    await page.evaluate(fixture);
    assert.equal(await page.evaluate(`Fixture.mount("", ${JSON.stringify(view)}, ${JSON.stringify(STATUS)})`), true, "the settings mount");
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

test("the master's settings keep the Account and Apply a master harness change picks while the reads after their saves run", { timeout: 120_000 }, async () => {
  const seen = await onSettings(APPLIED, async (page) => ({
    apply: await page.evaluate(`Fixture.pickOther("Apply a master harness change")`),
    account: await page.evaluate(`Fixture.pickOther("Account")`),
    idle: await page.evaluate(`Fixture.pickOther("Stop the master when idle after")`),
  }), "drawer");
  for (const [row, value] of Object.entries(seen)) {
    assert.notEqual(value, null, `${row} is in the drawer`);
    assert.equal(value.shown, value.picked, `${row} holds the pick`);
  }
});

test("each Stop the master when idle after choice reaches the engine under its own name and stays on screen (DEC-117 Q4)", { timeout: 120_000 }, async () => {
  const choices = ["one_hour", "off", "fifteen_minutes"];
  const seen = await onSettings(APPLIED, async (page) => {
    const picks = [];
    for (const choice of choices) picks.push(await page.evaluate(`Fixture.pickOther("Stop the master when idle after", ${JSON.stringify(choice)})`));
    return { picks, sent: await page.evaluate(`Fixture.sentTo("desktop_team_master_idle_stop")`) };
  }, "drawer");
  assert.deepEqual(seen.picks.map((pick) => pick?.shown ?? null), choices, "each pick stays shown");
  assert.deepEqual(seen.sent.args.map((call) => call.choice), choices, "each pick is sent by its name");
  assert.deepEqual([...new Set(seen.sent.args.map((call) => call.id))], ["alpha"], "the save names the project");
});

test("each Live terminal choice reaches the engine under its own value and stays on screen (DEC-117 Q4)", { timeout: 120000 }, async () => {
  const selector = JSON.stringify('[data-setting="Live terminal"] [role="switch"]');
  const seen = await onSettings(APPLIED, async (page) => {
    const state = () => page.evaluate(`document.querySelector(${selector})?.getAttribute("aria-checked") ?? null`);
    const before = await state();
    const picks = [];
    for (let turn = 0; turn < 2; turn += 1) {
      await page.evaluate(`Fixture.click(${selector}, "")`);
      await page.evaluate("Fixture.settle(240)");
      picks.push(await state());
    }
    return { before, picks, sent: await page.evaluate(`Fixture.sentTo("desktop_team_master_live_terminal")`) };
  }, "drawer");
  assert.equal(seen.before, "true", "on with nothing saved");
  assert.deepEqual(seen.picks, ["false", "true"], "each choice stays shown");
  assert.deepEqual(seen.sent.args.map((call) => call.on), [false, true], "each choice is sent");
  assert.deepEqual([...new Set(seen.sent.args.map((call) => call.id))], ["alpha"], "the save names the project");
});

const APP_SELECTS = [["project", "project-account-order", "balanced"], ["memory", "memory-agent-writes", "ask"]];

test("the app's own settings selects keep the pick while their reads run, and go back after a refused save", { timeout: 120_000 }, async () => {
  const seen = await withPage(async (page) => {
    await page.send("Emulation.setDeviceMetricsOverride", { width: 1440, height: 900, deviceScaleFactor: 1, mobile: false });
    await page.evaluate(appFixture);
    const rows = [];
    for (const [view, id, value] of APP_SELECTS) {
      await page.evaluate(`Fixture.mount(${JSON.stringify(view)}, false)`);
      const applied = await page.evaluate(`Fixture.pick(${JSON.stringify(id)}, ${JSON.stringify(value)})`);
      await page.evaluate(`Fixture.mount(${JSON.stringify(view)}, true)`);
      const refused = await page.evaluate(`Fixture.pick(${JSON.stringify(id)}, ${JSON.stringify(value)})`);
      rows.push({ id, value, applied, refused });
    }
    return rows;
  });
  for (const { id, value, applied, refused } of seen) {
    assert.equal(applied.asked, 1, `${id} asked the engine`);
    assert.equal(applied.shown, value, `${id} holds the pick`);
    assert.equal(refused.shown, refused.before, `${id} goes back after a refused save`);
  }
});

test("a refused account order save for a gone project shows its sentence on the project's settings, never the code (PROTO-4 F3)", { timeout: 120_000 }, async () => {
  const seen = await withPage(async (page) => {
    await page.evaluate(appFixture);
    await page.evaluate("Fixture.mountGone()");
    const picked = await page.evaluate(`Fixture.pick("project-account-order", "balanced")`);
    return { picked, shown: await page.evaluate("Fixture.shown()") };
  });
  assert.deepEqual(seen.shown.sent, ["desktop_set_account_schedule"]);
  assert.match(seen.shown.alert ?? "", /This project no longer exists\./);
  assert.doesNotMatch(seen.shown.alert ?? "", /project_not_found/);
  assert.equal(seen.picked.shown, seen.picked.before, "the refused pick goes back to the saved order");
});

async function press(page, key, code) {
  for (const type of ["keyDown", "keyUp"]) await page.send("Input.dispatchKeyEvent", { type, key, code: key, windowsVirtualKeyCode: code });
}

test("an arrow key on a segmented row moves the focus to the option it checked and goes on from the pick", { timeout: 120_000 }, async () => {
  const row = "Spread harnesses across accounts";
  const seen = await onSettings(APPLIED, async (page) => {
    assert.equal(await page.evaluate(`Fixture.focusChecked(${JSON.stringify(row)})`), true, "the checked option takes the focus");
    await press(page, "ArrowRight", 39);
    await page.evaluate("Fixture.settle(120)");
    const first = await page.evaluate(`Fixture.keyboard(${JSON.stringify(row)})`);
    await press(page, "ArrowRight", 39);
    await page.evaluate("Fixture.settle(120)");
    return { first, second: await page.evaluate(`Fixture.keyboard(${JSON.stringify(row)})`), sent: await page.evaluate(`Fixture.sentTo("desktop_team_spread_accounts")`) };
  });
  assert.deepEqual(seen.first, { checked: "On", focused: "On" }, "the focus follows the choice");
  assert.deepEqual(seen.second, { checked: "Off", focused: "Off" }, "the next arrow goes on from the pick");
  assert.deepEqual(seen.sent.args.map((call) => call.on), [true, false], "both picks are saved");
});

test("an arrow pressed while a save runs is dropped and the focus stays on the checked option", { timeout: 120_000 }, async () => {
  const row = "Workflow nudges";
  const seen = await onSettings(APPLIED, async (page) => {
    await page.evaluate(`Fixture.answerSaves(${JSON.stringify(APPLIED)}, 600)`);
    assert.equal(await page.evaluate(`Fixture.focusChecked(${JSON.stringify(row)})`), true);
    await press(page, "ArrowRight", 39);
    await page.evaluate("Fixture.settle(40)");
    await press(page, "ArrowRight", 39);
    await page.evaluate("Fixture.settle(40)");
    const during = await page.evaluate(`Fixture.keyboard(${JSON.stringify(row)})`);
    await page.evaluate("Fixture.settle(800)");
    return { during, after: await page.evaluate(`Fixture.keyboard(${JSON.stringify(row)})`), sent: await page.evaluate(`Fixture.sentTo("desktop_team_coordination")`) };
  });
  assert.deepEqual(seen.during, { checked: "Every 10 minutes", focused: "Every 10 minutes" });
  assert.deepEqual(seen.after, { checked: "Every 10 minutes", focused: "Every 10 minutes" });
  assert.equal(seen.sent.args.length, 1, "the dropped pick is not saved");
});

test("a select keeps the focus across its save, on the same element", { timeout: 120_000 }, async () => {
  const seen = await onSettings(APPLIED, async (page) => {
    const rows = [];
    for (const [row, id] of [["Token budget", "tg-token-budget-alpha"], ["Claude Code sub-agents", "tg-mode-claude-code-alpha"], ["Messages to Codex agents", "tg-delivery-codex-alpha"]]) {
      assert.equal(await page.evaluate(`Fixture.markFocus(${JSON.stringify(id)})`), true, `${row} takes the focus`);
      const picked = await page.evaluate(`Fixture.pickOther(${JSON.stringify(row)})`);
      rows.push({ row, picked, kept: await page.evaluate("Fixture.markedHasFocus()") });
    }
    return rows;
  });
  for (const { row, picked, kept } of seen) {
    assert.equal(picked.shown, picked.picked, `${row} holds the pick`);
    assert.equal(kept, true, `${row} keeps the focus`);
  }
});

test("the master's Account can go back to the old pool right after a save, and that pick reaches the engine", { timeout: 120_000 }, async () => {
  const seen = await onSettings(APPLIED, async (page) => {
    const first = await page.evaluate(`Fixture.pickOther("Account")`);
    const back = await page.evaluate(`Fixture.pickOther("Account", ${JSON.stringify(first.before)})`);
    return { first, back, assigned: await page.evaluate(`Fixture.sentTo("desktop_edit_model_access")`), recorded: await page.evaluate(`Fixture.sentTo("desktop_update_project_setup")`) };
  }, "drawer");
  assert.equal(seen.first.picked, "pool-b");
  assert.equal(seen.back.picked, "pool-a", "the second pick is the old pool");
  assert.equal(seen.back.shown, "pool-a");
  assert.deepEqual(seen.assigned.args.map((call) => Object.values(call).find((value) => value?.kind === "assign_project_pool").pool), ["pool-b", "pool-a"], "the old pool is assigned again");
  assert.deepEqual(seen.recorded.args.map((call) => call.setup.pool), ["pool-b", "pool-a"], "the project details follow");
  assert.deepEqual(seen.recorded.args.map((call) => call.revision), [4, 5], "the second record names the revision the first one left");
});

test("a help line follows the held pick, not the value the save replaced", { timeout: 120_000 }, async () => {
  const budget = await onSettings(APPLIED, async (page) => {
    const before = await page.evaluate(`Fixture.helperOf("tg-token-budget-alpha")`);
    await page.evaluate(`Fixture.pickOther("Token budget")`);
    return { before, after: await page.evaluate(`Fixture.helperOf("tg-token-budget-alpha")`) };
  });
  assert.match(budget.before, /^Off lets the agents/);
  assert.match(budget.after, /^Used /, "the budget's line shows while its pick is held");
});

test("when two picks save at once, the answer to the older one never puts it back on screen", { timeout: 120_000 }, async () => {
  const seen = await withPage(async (page) => {
    await page.evaluate(appFixture);
    await page.evaluate(`Fixture.mount("memory", false, true)`);
    const first = await page.evaluate(`Fixture.pick("memory-agent-writes", "ask")`);
    const second = await page.evaluate(`Fixture.pick("memory-agent-writes", "save")`);
    await page.evaluate("Fixture.answer(1)");
    await page.evaluate("Fixture.answer(0)");
    return { first, second, last: await page.evaluate(`document.getElementById("memory-agent-writes").value`) };
  });
  assert.equal(seen.second.asked, 2, "both picks asked the engine");
  assert.equal(seen.last, "save", "the latest pick stays");
});

test("the Process page's Delivery rows and the Update channel keep the pick while their reads run, and go back after a refused save", { timeout: 120_000 }, async () => {
  const seen = await withPage(async (page) => {
    await page.evaluate(appFixture);
    const rows = {};
    for (const [view, label] of [["delivery", "Instructions agents get"], ["delivery", "When you edit instructions"], ["updates", "Update channel"]]) {
      await page.evaluate(`Fixture.mount(${JSON.stringify(view)}, false)`);
      const applied = await page.evaluate(`Fixture.pickSegment(${JSON.stringify(label)})`);
      await page.evaluate(`Fixture.mount(${JSON.stringify(view)}, true)`);
      rows[label] = { applied, refused: await page.evaluate(`Fixture.pickSegment(${JSON.stringify(label)})`) };
    }
    await page.evaluate(`Fixture.mount("delivery", false)`);
    const applied = await page.evaluate(`Fixture.flip("agent-transcripts")`);
    await page.evaluate(`Fixture.mount("delivery", true)`);
    rows["Keep transcripts"] = { applied: { ...applied, picked: "false" }, refused: await page.evaluate(`Fixture.flip("agent-transcripts")`) };
    return rows;
  });
  for (const [row, { applied, refused }] of Object.entries(seen)) {
    assert.equal(applied.asked, 1, `${row} asked the engine`);
    assert.equal(applied.shown, applied.picked, `${row} holds the pick`);
    assert.equal(refused.shown, refused.before, `${row} goes back after a refused save`);
  }
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
