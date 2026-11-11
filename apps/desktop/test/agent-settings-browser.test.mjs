import "./load-source.mjs";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { bundle, withPage } from "./browser.mjs";
import { press } from "./settings-tab-steps.mjs";

const fixture = await bundle(new URL("./fixtures/team-settings-layout.mjs", import.meta.url).pathname);

const CSS = [
  readFileSync(new URL("../src/ui/tokens.css", import.meta.url), "utf8"),
  readFileSync(new URL("../src/style.css", import.meta.url), "utf8").replace(/@import[^;]*;/g, ""),
  readFileSync(new URL("../src/screens/project/projects.css", import.meta.url), "utf8"),
  readFileSync(new URL("../src/screens/team/team-graph.css", import.meta.url), "utf8"),
].join("\n");
const harness = (name, role) => ({ harness: name, role, agents_max: 2, tools: true, live: 0, delegation: null, delegation_label: null, runnable: true });
const STATUS = {
  project_id: "alpha", keep_warm: false, idle_stop_secs: 600, paused: false, min_agents: 1, max_agents: 3, settings_version: 1, master_lands: "merges",
  harnesses: [harness("codex", "master"), harness("codex", "sub"), harness("claude", "sub")], agents: [], tasks: [], messages: [], plan: [],
  events: { recent: [], waiting: [], notifications: [], paused: false, questions: [],
    wake: { app: null, app_version: 0, project: null, project_version: 0, agents: [], cleared: [], effective: { question: true, finished: true, failed: true, change_set_ready: true, progress: false, hourly_cap: 30 }, level: "shipped" } },
};
const GROUPS = [
  ["Model and harness", ["Master harness", "Sub-agent harnesses", "Keep sub-agents ready", "Reuse idle agents", "On master harness switch", "Spread harnesses across accounts", "Max harnesses per project", "Max teammates per harness"]],
  ["Access and merging", ["Agent access", "Under Full access, merging into main", "Ask me before merging into the master's branch", "Agent branches", "After you accept, the agent's branch restarts from main", "Master's branch", "Land on", "Sub-agent work lands on its lead's branch", "Refuse a change set when one new folder adds more than"]],
  ["Skills and tools", ["Skills per role", "MCP servers", "Workflow nudges", "Task-start reminder", "Counted tools", "Re-ask a sub-agent's empty reply"]],
  ["Coordination", ["Claude Code sub-agents", "Codex sub-agents", "rcli sub-agents", "Sub-agent depth", "Sub-agent questions go to", "Overlapping edits", "Terminals per window", "Catch up with main", "Finished agents", "Terminal display", "Messages to Claude Code agents", "Messages to Codex agents", "Messages to rcli agents"]],
  ["Limits", ["Each task may run", "Each agent session may run", "Stop an idle sub-agent after", "Background commands at task end", "Token budget"]],
];
const MASTER = [
  ["Master", ["Harness", "Account", "Live terminal"]],
  ["Conversation", ["On master harness switch", "Stop the master when idle after", "Apply a master harness change"]],
  ["Messages", ["Sub-agent questions go to", "Messages to Claude Code agents", "Messages to Codex agents", "Messages to rcli agents", "Re-ask a sub-agent's empty reply"]],
  ["Access", ["Full access"]],
];
const READ_ONLY = ["MCP servers", "After you accept, the agent's branch restarts from main", "rcli sub-agents", "Messages to rcli agents"];

/** Presses Tab until every wanted row had the focus or the presses run out. Inputs: the page's
 * handles and the row labels. Output: the rows the focus reached, in order, once each. */
async function tabThrough({ evaluate, send }, wanted, presses = 160) {
  await evaluate("document.activeElement?.blur(), true");
  const reached = [];
  for (let press = 0; press < presses && !wanted.every((row) => reached.includes(row)); press += 1) {
    for (const type of ["keyDown", "keyUp"]) await send("Input.dispatchKeyEvent", { type, key: "Tab", code: "Tab", windowsVirtualKeyCode: 9 });
    const row = await evaluate("Fixture.focused()");
    if (row !== null && !reached.includes(row)) reached.push(row);
  }
  return reached;
}

for (const width of [1024, 1440]) {
  test(`at a ${width} px window every Agent setting sits in its group panel, nothing runs past a panel and Tab reaches each one`, { timeout: 120_000 }, async () => {
    const seen = await withPage(async (page) => {
      await page.send("Emulation.setDeviceMetricsOverride", { width, height: 900, deviceScaleFactor: 1, mobile: false });
      await page.evaluate(fixture);
      const mounted = await page.evaluate(`Fixture.mount(${JSON.stringify(CSS)}, "page", ${JSON.stringify(STATUS)})`);
      const measured = await page.evaluate("Fixture.measure()");
      const reached = await tabThrough(page, GROUPS.flatMap(([, rows]) => rows).filter((row) => !READ_ONLY.includes(row)));
      return { mounted, measured, reached };
    });
    assert.equal(seen.mounted, true, "Agent settings mount");
    assert.deepEqual(seen.measured.groups, GROUPS.map(([title, rows]) => ({ title, rows })), "each row is drawn inside its own group's panel, in order");
    assert.ok(seen.measured.width > 500, `the groups have room, ${seen.measured.width} px`);
    assert.deepEqual(seen.measured.past, [], "nothing runs past a group's panel");
    assert.ok(seen.measured.sideways <= 0, `the page does not scroll sideways, ${seen.measured.sideways} px`);
    assert.ok(seen.measured.help.rows >= 25, `rows with a label, a help line and a control are measured, ${seen.measured.help.rows}`);
    assert.deepEqual(seen.measured.help.apart, [], "each row's help line sits under its label, left of the control");
    assert.deepEqual(GROUPS.flatMap(([, rows]) => rows).filter((row) => !READ_ONLY.includes(row) && !seen.reached.includes(row)), [], "Tab reaches every setting with a control");
  });
}

test("the master's settings drawer holds its four groups with nothing past the drawer, and Tab reaches each control", { timeout: 120_000 }, async () => {
  const seen = await withPage(async (page) => {
    await page.send("Emulation.setDeviceMetricsOverride", { width: 1024, height: 900, deviceScaleFactor: 1, mobile: false });
    await page.evaluate(fixture);
    const mounted = await page.evaluate(`Fixture.mount(${JSON.stringify(CSS)}, "drawer", ${JSON.stringify(STATUS)})`);
    const measured = await page.evaluate("Fixture.measure()");
    const controls = MASTER.flatMap(([, rows]) => rows).filter((row) => !READ_ONLY.includes(row));
    const reached = await tabThrough(page, controls);
    return { mounted, measured, reached, controls };
  });
  assert.equal(seen.mounted, true, "the master's settings mount");
  assert.deepEqual(seen.measured.groups, MASTER.map(([title, rows]) => ({ title, rows })));
  assert.deepEqual(seen.measured.past, [], "nothing runs past a group's panel");
  assert.ok(seen.measured.sideways <= 0, `the drawer does not scroll sideways, ${seen.measured.sideways} px`);
  assert.ok(seen.measured.help.rows >= 3, `drawer rows with a label, a help line and a control are measured, ${seen.measured.help.rows}`);
  assert.deepEqual(seen.measured.help.apart, [], "each drawer row's help line sits under its label, left of the control");
  assert.deepEqual(seen.controls.filter((row) => !seen.reached.includes(row)), [], "Tab reaches every control in the drawer");
});

test("two quick coordination changes from different rows both stay saved", { timeout: 120_000 }, async () => {
  const seen = await withPage(async (page) => {
    await page.send("Emulation.setDeviceMetricsOverride", { width: 1440, height: 900, deviceScaleFactor: 1, mobile: false });
    await page.evaluate(fixture);
    const mounted = await page.evaluate(`Fixture.mount(${JSON.stringify(CSS)}, "page", ${JSON.stringify(STATUS)})`);
    const clicked = await page.evaluate(`Fixture.click('[aria-label="Overlapping edits"] [role="radio"]', "Block the later one") && Fixture.click('[aria-label="Workflow nudges"] [role="radio"]', "Every 10 minutes")`);
    await page.evaluate("Fixture.settle(600)");
    return { mounted, clicked, sent: await page.evaluate(`Fixture.sentTo("desktop_team_coordination")`) };
  });
  assert.equal(seen.mounted, true, "Agent settings mount");
  assert.equal(seen.clicked, true, "both choices are there to click");
  assert.equal(seen.sent.most, 1, "one coordination save runs at a time");
  assert.deepEqual(seen.sent.args.map((call) => [call.settings.overlaps, call.settings.nudges]), [["block", "off"], ["block", "ten_minutes"]], "the nudges save keeps Block");
});

test("a coordination save keeps a change the TUI or another window made after this window's save, while the rows still show the older read", { timeout: 120_000 }, async () => {
  const seen = await withPage(async (page) => {
    await page.send("Emulation.setDeviceMetricsOverride", { width: 1440, height: 900, deviceScaleFactor: 1, mobile: false });
    await page.evaluate(fixture);
    const mounted = await page.evaluate(`Fixture.mount(${JSON.stringify(CSS)}, "page", ${JSON.stringify(STATUS)})`);
    const nudges = await page.evaluate(`Fixture.click('[aria-label="Workflow nudges"] [role="radio"]', "Every 10 minutes")`);
    await page.evaluate("Fixture.settle(300)");
    await page.evaluate(`Fixture.changedElsewhere({ overlaps: "block" })`);
    const reminder = await page.evaluate(`Fixture.click('[aria-label="Task-start reminder"] [role="radio"]', "Off")`);
    await page.evaluate("Fixture.settle(300)");
    return { mounted, nudges, reminder, sent: await page.evaluate(`Fixture.sentTo("desktop_team_coordination")`) };
  });
  assert.equal(seen.mounted, true, "Agent settings mount");
  assert.deepEqual([seen.nudges, seen.reminder], [true, true], "both choices are there to click");
  assert.deepEqual(seen.sent.args.map((call) => [call.settings.overlaps, call.settings.nudges, call.settings.task_start]),
    [["warn", "ten_minutes", "on"], ["block", "ten_minutes", "off"]], "the reminder save keeps the Block set elsewhere");
});

test("Agent access on a project made before setup says so, not that the project is gone", { timeout: 120_000 }, async () => {
  const said = await withPage(async (page) => {
    await page.send("Emulation.setDeviceMetricsOverride", { width: 1440, height: 900, deviceScaleFactor: 1, mobile: false });
    await page.evaluate(fixture);
    await page.evaluate(`Fixture.refusePolicy("project_setup_missing")`);
    assert.equal(await page.evaluate(`Fixture.mount(${JSON.stringify(CSS)}, "page", ${JSON.stringify(STATUS)})`), true);
    await page.evaluate("Fixture.settle(150)");
    return page.evaluate(`document.querySelector('[data-setting="Agent access"]').textContent`);
  });
  assert.match(said, /This project was created before project setup existed\./);
  assert.doesNotMatch(said, /no longer exists/);
});

test("Agent access in place moves its choice on arrow keys and clicks, says first when saving stops agents, and saves only on Save, Full access after its warning", { timeout: 120_000 }, async () => {
  const sentAccess = `Fixture.sentTo("desktop_set_project_agent_policy").args.map((call) => call.policy.access)`;
  const rowText = `document.querySelector('[data-setting="Agent access"]').textContent`;
  const checked = `document.querySelector('[aria-label="Agent access"] [aria-checked="true"]')?.textContent ?? null`;
  const seen = await withPage(async (page) => {
    await page.send("Emulation.setDeviceMetricsOverride", { width: 1440, height: 900, deviceScaleFactor: 1, mobile: false });
    await page.evaluate(fixture);
    const mounted = await page.evaluate(`Fixture.mount(${JSON.stringify(CSS)}, "page", ${JSON.stringify(STATUS)})`);
    const pressed = await page.evaluate(`Fixture.press('[aria-label="Agent access"]', "ArrowRight")`);
    await page.evaluate("Fixture.settle(150)");
    const afterArrow = { sent: await page.evaluate(sentAccess), checked: await page.evaluate(checked) };
    const picked = await page.evaluate(`Fixture.click('[aria-label="Agent access"] [role="radio"]', "Own branch")`);
    await page.evaluate("Fixture.settle(150)");
    const afterClick = { sent: await page.evaluate(sentAccess), checked: await page.evaluate(checked), row: await page.evaluate(rowText) };
    const saved = await page.evaluate(`Fixture.click('[data-setting="Agent access"] button', "Save agent access")`);
    await page.evaluate("Fixture.settle(300)");
    const afterSave = { sent: await page.evaluate(sentAccess), checked: await page.evaluate(checked), row: await page.evaluate(rowText) };
    await page.evaluate(`Fixture.click('[aria-label="Agent access"] [role="radio"]', "Full access")`);
    await page.evaluate("Fixture.settle(150)");
    await page.evaluate(`Fixture.click('[data-setting="Agent access"] button', "Save agent access")`);
    await page.evaluate("Fixture.settle(150)");
    const asked = { sent: await page.evaluate(sentAccess), dialog: await page.evaluate(`document.querySelector("dialog[open] h2")?.textContent ?? null`) };
    await page.evaluate(`Fixture.click("dialog[open] button", "Allow Full access")`);
    await page.evaluate("Fixture.settle(300)");
    const afterFull = { sent: await page.evaluate(sentAccess), checked: await page.evaluate(checked) };
    return { mounted, pressed, afterArrow, picked, afterClick, saved, afterSave, asked, afterFull };
  });
  assert.equal(seen.mounted, true, "Agent settings mount");
  assert.equal(seen.pressed, true, "the Agent access choice is there");
  assert.deepEqual(seen.afterArrow, { sent: [], checked: "Sandboxed" }, "an arrow key wrapping Full access to Sandboxed only moves the choice");
  assert.equal(seen.picked, true, "Own branch is there to click");
  assert.deepEqual([seen.afterClick.sent, seen.afterClick.checked], [[], "Own branch"], "a click only moves the choice");
  assert.match(seen.afterClick.row, /Saving lowers agent access from Full access to Own branch: agents running above Own branch stop and restart at it\./, "the row says the save stops agents before Save");
  assert.equal(seen.saved, true, "a moved choice shows Save agent access");
  assert.deepEqual([seen.afterSave.sent, seen.afterSave.checked], [["own_branch"], "Own branch"], "Save sends the choice once and the row shows the saved level");
  assert.doesNotMatch(seen.afterSave.row, /Save agent access|Saving lowers/, "nothing is left to save");
  assert.deepEqual(seen.asked, { sent: ["own_branch"], dialog: "Give this project's agents Full access?" }, "Save of Full access asks first");
  assert.deepEqual(seen.afterFull, { sent: ["own_branch", "full"], checked: "Full access" }, "Allow Full access saves it");
});

test("the Agents tab opens Agent settings at the routed group, and its view switch moves between Team and Agent settings", { timeout: 120_000 }, async () => {
  const seen = await withPage(async (page) => {
    await page.send("Emulation.setDeviceMetricsOverride", { width: 1440, height: 900, deviceScaleFactor: 1, mobile: false });
    await page.evaluate(fixture);
    const mounted = await page.evaluate(`Fixture.mount(${JSON.stringify(CSS)}, "agents", ${JSON.stringify(STATUS)}, "access")`);
    const routed = await page.evaluate("Fixture.shown()");
    const toTeam = await page.evaluate(`Fixture.click('[aria-label="Agents view"] [role="radio"]', "Team")`);
    await page.evaluate("Fixture.settle(50)");
    const teamView = await page.evaluate("Fixture.shown()");
    const toSettings = await page.evaluate(`Fixture.click('[aria-label="Agents view"] [role="radio"]', "Agent settings")`);
    await page.evaluate("Fixture.settle(50)");
    const settingsView = await page.evaluate("Fixture.shown()");
    await page.evaluate(`Fixture.mount(${JSON.stringify(CSS)}, "agents", ${JSON.stringify(STATUS)}, null)`);
    const plain = await page.evaluate("Fixture.shown()");
    return { mounted, routed, toTeam, teamView, toSettings, settingsView, plain };
  });
  assert.equal(seen.mounted, true, "the Agents tab mounts");
  assert.deepEqual({ ...seen.routed, drawer: undefined }, { view: "Agent settings", settings: true, stopAll: false, focused: "set-alpha-access-title", drawer: undefined }, "agents?section=access opens Agent settings at Access and merging");
  assert.equal(seen.toTeam, true);
  assert.deepEqual([seen.teamView.view, seen.teamView.settings, seen.teamView.stopAll], ["Team", false, true], "Team shows the team's actions and no settings");
  assert.equal(seen.toSettings, true);
  assert.deepEqual([seen.settingsView.view, seen.settingsView.settings, seen.settingsView.stopAll], ["Agent settings", true, false]);
  assert.deepEqual([seen.plain.view, seen.plain.settings], ["Team", false], "a route with no section opens the Team view");
});

test("Master settings opens its drawer with the master's four groups, and Close shuts it", { timeout: 120_000 }, async () => {
  const seen = await withPage(async (page) => {
    await page.send("Emulation.setDeviceMetricsOverride", { width: 1024, height: 900, deviceScaleFactor: 1, mobile: false });
    await page.evaluate(fixture);
    const mounted = await page.evaluate(`Fixture.mount(${JSON.stringify(CSS)}, "button", ${JSON.stringify(STATUS)})`);
    const before = await page.evaluate("Fixture.shown().drawer");
    const opened = await page.evaluate(`Fixture.click("button", "Master settings")`);
    await page.evaluate("Fixture.settle(50)");
    const open = await page.evaluate("Fixture.shown().drawer");
    const closed = await page.evaluate(`Fixture.click('dialog.drawer button[aria-label="Close"]', "")`);
    await page.evaluate("Fixture.settle(50)");
    const after = await page.evaluate("Fixture.shown().drawer");
    return { mounted, before, opened, open, closed, after };
  });
  assert.equal(seen.mounted, true, "the Master settings button mounts");
  assert.equal(seen.before, null, "no drawer before the button");
  assert.equal(seen.opened, true);
  assert.deepEqual(seen.open, { title: "Master settings", groups: MASTER.map(([title]) => title) });
  assert.equal(seen.closed, true);
  assert.equal(seen.after, null, "Close shuts the drawer");
});

test("Esc on the No pool question closes only the question, and the focus goes back to the select", { timeout: 120_000 }, async () => {
  const seen = await withPage(async (page) => {
    await page.send("Emulation.setDeviceMetricsOverride", { width: 1024, height: 900, deviceScaleFactor: 1, mobile: false });
    await page.evaluate(fixture);
    await page.evaluate(`Fixture.mount(${JSON.stringify(CSS)}, "button", ${JSON.stringify(STATUS)})`);
    await press(page, "button", "Master settings");
    await press(page, "dialog.drawer h2");
    await page.evaluate(`Fixture.markFocus("master-set-alpha-account")`);
    await page.evaluate(`Fixture.pickOther("Account", "")`);
    const asked = await page.evaluate(`document.querySelector("dialog.dialog[open] h2")?.textContent ?? null`);
    for (const type of ["keyDown", "keyUp"]) await page.send("Input.dispatchKeyEvent", { type, key: "Escape", code: "Escape", windowsVirtualKeyCode: 27 });
    await page.evaluate("Fixture.settle(50)");
    return {
      asked,
      question: await page.evaluate(`document.querySelector("dialog.dialog[open]") !== null`),
      drawer: await page.evaluate("Fixture.shown().drawer?.title ?? null"),
      focus: await page.evaluate("Fixture.markedHasFocus()"),
      sent: await page.evaluate(`Fixture.sentTo("desktop_edit_model_access")`),
    };
  });
  assert.equal(seen.asked, "Use no pool for this project?");
  assert.equal(seen.question, false, "Esc closes the question");
  assert.equal(seen.drawer, "Master settings", "Master settings stays open");
  assert.equal(seen.focus, true, "the focus goes back to the select that asked");
  assert.deepEqual(seen.sent.args, [], "saying no changes nothing");
});

test("a master harness save holds the sub-agent rows until the read, so a quick change there sends no second save", { timeout: 120_000 }, async () => {
  const seen = await withPage(async (page) => {
    await page.send("Emulation.setDeviceMetricsOverride", { width: 1440, height: 900, deviceScaleFactor: 1, mobile: false });
    await page.evaluate(fixture);
    await page.evaluate(`Fixture.mount(${JSON.stringify(CSS)}, "page", ${JSON.stringify(STATUS)})`);
    await page.evaluate(`Fixture.answerSaves({ outcome: "applied", pending: false, rejection: null })`);
    const picked = await page.evaluate(`Fixture.pickOther("Master harness", "claude")`);
    const held = await page.evaluate(`document.querySelector('button[aria-label="More Codex agents"]').disabled`);
    const clicked = await page.evaluate(`Fixture.click('button[aria-label="More Codex agents"]', "+")`);
    await page.evaluate("Fixture.settle(120)");
    return { picked, held, clicked, sent: await page.evaluate(`Fixture.sentTo("desktop_update_project_setup")`) };
  });
  assert.equal(seen.picked.shown, "claude", "the master row shows the change");
  assert.equal(seen.clicked, true, "the sub-agent stepper is there to press");
  assert.equal(seen.held, true, "the sub-agent row waits for the read too");
  assert.equal(seen.sent.args.length, 1, "one save, so the engine refuses none as stale");
});

test("a refused master harness save says why under that row only", { timeout: 120_000 }, async () => {
  const seen = await withPage(async (page) => {
    await page.send("Emulation.setDeviceMetricsOverride", { width: 1440, height: 900, deviceScaleFactor: 1, mobile: false });
    await page.evaluate(fixture);
    await page.evaluate(`Fixture.mount(${JSON.stringify(CSS)}, "page", ${JSON.stringify(STATUS)})`);
    await page.evaluate(`Fixture.answerSaves({ outcome: "rejected", pending: false, rejection: "stale_revision" })`);
    await page.evaluate(`Fixture.pickOther("Master harness", "claude")`);
    return page.evaluate(`["Master harness", "Sub-agent harnesses"].map((row) => document.querySelectorAll('[data-setting="' + row + '"] [role="alert"]').length)`);
  });
  assert.deepEqual(seen, [1, 0]);
});
