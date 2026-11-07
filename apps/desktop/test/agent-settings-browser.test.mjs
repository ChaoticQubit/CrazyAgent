import "./load-source.mjs";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { bundle, withPage } from "./browser.mjs";

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
};
const GROUPS = [
  ["Model and harness", ["Master and sub-agent harnesses", "Keep sub-agents ready", "On master harness switch", "Spread harnesses across accounts", "Max harnesses per project", "Max teammates per harness"]],
  ["Access and merging", ["Agent access", "When the master resolves conflicts", "Ask me before merging into the master's branch", "Agent branches", "Master's branch", "Land on", "Sub-agent work lands on its lead's branch", "Refuse a change set when one new folder adds more than"]],
  ["Skills and tools", ["Skills per role", "Workflow nudges", "Task-start reminder", "Counted tools", "Re-ask a sub-agent's empty reply"]],
  ["Coordination", ["Claude Code sub-agents", "Messages to Claude Code agents", "Claude Code terminal display", "Codex sub-agents", "Messages to Codex agents", "rcli sub-agents", "Messages to rcli agents", "Sub-agent depth", "Sub-agent questions go to", "Overlapping edits", "Catch up with main", "Finished agents"]],
  ["Limits", ["Each task may run", "Each agent session may run", "Stop an idle sub-agent after", "Background commands at task end", "Token budget"]],
];
const MASTER = [
  ["Master", ["Harness", "Account"]],
  ["Conversation", ["On master harness switch", "Apply a master harness change", "Stop the master when idle after"]],
  ["Messages", ["Sub-agent questions go to", "Messages to Claude Code agents", "Re-ask a sub-agent's empty reply"]],
  ["Access", ["Full access"]],
];

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
      const reached = await tabThrough(page, GROUPS.flatMap(([, rows]) => rows));
      return { mounted, measured, reached };
    });
    assert.equal(seen.mounted, true, "Agent settings mount");
    assert.deepEqual(seen.measured.groups, GROUPS.map(([title, rows]) => ({ title, rows })), "each row is drawn inside its own group's panel, in order");
    assert.ok(seen.measured.width > 500, `the groups have room, ${seen.measured.width} px`);
    assert.deepEqual(seen.measured.past, [], "nothing runs past a group's panel");
    assert.ok(seen.measured.sideways <= 0, `the page does not scroll sideways, ${seen.measured.sideways} px`);
    assert.deepEqual(GROUPS.flatMap(([, rows]) => rows).filter((row) => !seen.reached.includes(row)), [], "Tab reaches every setting");
  });
}

test("the master's settings drawer holds its four groups with nothing past the drawer, and Tab reaches each control", { timeout: 120_000 }, async () => {
  const seen = await withPage(async (page) => {
    await page.send("Emulation.setDeviceMetricsOverride", { width: 1024, height: 900, deviceScaleFactor: 1, mobile: false });
    await page.evaluate(fixture);
    const mounted = await page.evaluate(`Fixture.mount(${JSON.stringify(CSS)}, "drawer", ${JSON.stringify(STATUS)})`);
    const measured = await page.evaluate("Fixture.measure()");
    const controls = MASTER.flatMap(([, rows]) => rows).filter((row) => row !== "Stop the master when idle after");
    const reached = await tabThrough(page, controls);
    return { mounted, measured, reached, controls };
  });
  assert.equal(seen.mounted, true, "the master's settings mount");
  assert.deepEqual(seen.measured.groups, MASTER.map(([title, rows]) => ({ title, rows })));
  assert.deepEqual(seen.measured.past, [], "nothing runs past a group's panel");
  assert.ok(seen.measured.sideways <= 0, `the drawer does not scroll sideways, ${seen.measured.sideways} px`);
  assert.deepEqual(seen.controls.filter((row) => !seen.reached.includes(row)), [], "Tab reaches every control in the drawer");
});
