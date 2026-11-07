import "./load-source.mjs";
import assert from "node:assert/strict";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";

const controls = await import("../src/screens/team/controls.tsx");
const client = await import("../src/engine-client.ts");
const conversation = await import("../src/screens/project/conversation.tsx");
const agents = await import("../src/screens/project/agents.tsx");

const GROUPS = [
  ["model", "Model and harness", ["Master and sub-agent harnesses", "Keep sub-agents ready", "On master harness switch", "Spread harnesses across accounts", "Max harnesses per project", "Max teammates per harness"]],
  ["access", "Access and merging", ["Agent access", "When the master resolves conflicts", "Ask me before merging into the master's branch", "Agent branches", "Master's branch", "Land on", "Sub-agent work lands on its lead's branch", "Refuse a change set when one new folder adds more than"]],
  ["tools", "Skills and tools", ["Skills per role", "Workflow nudges", "Task-start reminder", "Counted tools", "Re-ask a sub-agent's empty reply"]],
  ["coord", "Coordination", ["Claude Code sub-agents", "Messages to Claude Code agents", "Claude Code terminal display", "Codex sub-agents", "Messages to Codex agents", "rcli sub-agents", "Messages to rcli agents", "Sub-agent depth", "Sub-agent questions go to", "Overlapping edits", "Catch up with main", "Finished agents"]],
  ["limits", "Limits", ["Each task may run", "Each agent session may run", "Stop an idle sub-agent after", "Background commands at task end", "Token budget"]],
];
const MASTER_GROUPS = [
  ["Master", ["Harness", "Account"]],
  ["Conversation", ["On master harness switch", "Apply a master harness change", "Stop the master when idle after"]],
  ["Messages", ["Sub-agent questions go to", "Messages to Claude Code agents", "Re-ask a sub-agent's empty reply"]],
  ["Access", ["Full access"]],
];

const attr = (text) => text.replace(/&/g, "&amp;").replace(/"/g, "&quot;").replace(/'/g, "&#x27;");
const harness = (name, role, extra = {}) => ({ harness: name, role, agents_max: 2, tools: true, live: 0, delegation: null, delegation_label: null, runnable: true, ...extra });
const status = (extra = {}) => ({
  project_id: "chores", keep_warm: false, idle_stop_secs: 600, paused: false, min_agents: 1, max_agents: 3,
  harnesses: [harness("codex", "master"), harness("codex", "sub"), harness("claude", "sub")],
  agents: [], tasks: [], messages: [], plan: [], ...extra,
});
const settingsHtml = (extra = {}) => renderToStaticMarkup(createElement(controls.TeamControls, { projectId: "chores", status: status(extra), reload() {} }));

/** Cuts rendered settings into their groups. Input: the markup. Output: each group's heading
 * and the markup up to the next group. */
function groups(html) {
  return html.split('<section class="set-group"').slice(1).map((body) => ({ title: body.match(/<h[23][^>]*>([^<]*)<\/h[23]>/)?.[1] ?? null, body }));
}

/** The choices a radio group offers and the one picked. Inputs: the markup and the group's
 * label. Output: the labels in order and the checked one. */
function radios(html, label) {
  const group = html.match(new RegExp(`role="radiogroup" aria-label="${attr(label)}">([\\s\\S]*?)</div>`));
  assert.ok(group, `a radio group named ${label}`);
  const choices = [...group[1].matchAll(/role="radio" aria-checked="(true|false)"[^>]*>([^<]*)<\/button>/g)];
  return { labels: choices.map((choice) => choice[2]), checked: choices.filter((choice) => choice[1] === "true").map((choice) => choice[2]) };
}

test("Agent settings sit in the mockup's five groups, in its order, each with a heading and a purpose line", () => {
  assert.ok(Array.isArray(controls.SETTING_GROUPS), "controls.tsx lists the setting groups");
  assert.deepEqual(controls.SETTING_GROUPS.map((group) => [group.id, group.title]), GROUPS.map(([id, title]) => [id, title]));
  for (const group of controls.SETTING_GROUPS) assert.ok(group.about.length > 0, `${group.title} says what it is for`);
  const found = groups(settingsHtml());
  assert.deepEqual(found.map((group) => group.title), GROUPS.map(([, title]) => title));
  controls.SETTING_GROUPS.forEach((group, index) => assert.ok(found[index].body.includes(attr(group.about)), `${group.title} shows its purpose line`));
  assert.match(settingsHtml(), /aria-label="Agent settings"/);
});

test("every setting row sits under its own group heading and under no other", () => {
  const found = groups(settingsHtml({ master_lands: "merges" }));
  assert.equal(found.length, GROUPS.length, "one section per group");
  GROUPS.forEach(([, title, rows], index) => {
    for (const row of rows) {
      const marker = `data-setting="${attr(row)}"`;
      assert.ok(found[index].body.includes(marker), `${row} is under ${title}`);
      found.forEach((other, at) => { if (at !== index) assert.ok(!other.body.includes(marker), `${row} is not under ${other.title}`); });
    }
  });
  assert.match(found[1].body, />Branches</, "Access and merging has its Branches part");
  assert.ok(Array.isArray(controls.SETTING_GROUPS), "controls.tsx lists the setting groups");
  assert.deepEqual(controls.SETTING_GROUPS.map((group) => group.rows), GROUPS.map(([, , rows]) => rows), "Find a setting and the counts know every row");
});

test("the group list names each group with its row count, one button each", () => {
  const html = settingsHtml();
  const nav = html.match(/<nav[^>]*aria-label="Setting groups"[^>]*>([\s\S]*?)<\/nav>/);
  assert.ok(nav, "a list of the setting groups");
  const buttons = [...nav[1].matchAll(/<button type="button"[^>]*>([\s\S]*?)<\/button>/g)].map((button) => button[1].replace(/<[^>]+>/g, " ").replace(/\s+/g, " ").trim());
  assert.deepEqual(buttons, GROUPS.map(([, title, rows]) => `${title} ${rows.length}`));
  assert.match(html, /aria-label="Find a setting"/);
});

test("Find a setting keeps the rows whose label matches and the groups that hold them", () => {
  assert.equal(typeof controls.matchingGroups, "function", "controls.tsx has matchingGroups");
  assert.deepEqual(controls.matchingGroups("budget").map((group) => [group.id, group.rows]), [["limits", ["Token budget"]]]);
  assert.deepEqual(controls.matchingGroups("  MAIN ").map((group) => [group.id, group.rows]), [["coord", ["Catch up with main"]]]);
  assert.deepEqual(controls.matchingGroups("").map((group) => group.rows), GROUPS.map(([, , rows]) => rows));
  assert.deepEqual(controls.matchingGroups("no such setting"), []);
});

test("Overlapping edits lists the engine's choices in their order and shows an unknown engine value as it is", () => {
  assert.deepEqual(client.OVERLAP_CHOICES, ["warn", "block", "off"]);
  const coordination = (overlaps) => ({ settings: { questions_to: "lead", overlaps, nudges: "off", counted_tools: [], role_skills: { master: null, lead: null, worker: null }, claude_display: "classic", task_start: "on" }, harnesses: [] });
  assert.deepEqual(radios(settingsHtml({ coordination: coordination("block") }), "Overlapping edits"), { labels: ["Warn", "Block", "Off"], checked: ["Block"] });
  assert.deepEqual(radios(settingsHtml({ coordination: coordination("wait") }), "Overlapping edits"), { labels: ["Warn", "Block", "Off", "wait"], checked: ["wait"] });
});

test("the Team view keeps the team's actions, and Agent settings holds none of them", () => {
  assert.equal(typeof controls.TeamActions, "function", "controls.tsx has TeamActions");
  const actions = renderToStaticMarkup(createElement(controls.TeamActions, { projectId: "chores", status: status(), reload() {} }));
  assert.match(actions, />Stop all</);
  assert.match(actions, /<h4>Hand a task to a sub-agent<\/h4>/);
  assert.doesNotMatch(actions, /class="set-group"/);
  const settings = settingsHtml();
  assert.doesNotMatch(settings, /Hand a task to a sub-agent/);
  assert.doesNotMatch(settings, />Stop all</);
});

test("the Agents tab opens Agent settings from the route, at a group when the route names one", () => {
  assert.equal(typeof agents.agentsView, "function", "agents.tsx has agentsView");
  assert.deepEqual(agents.agentsView(undefined), { view: "team", group: null });
  assert.deepEqual(agents.agentsView("settings"), { view: "settings", group: null });
  assert.deepEqual(agents.agentsView("coord"), { view: "settings", group: "coord" });
  assert.deepEqual(agents.agentsView("access"), { view: "settings", group: "access" });
  assert.deepEqual(agents.agentsView("bogus"), { view: "team", group: null });
});

test("the Master settings drawer holds Master, Conversation, Messages and Access in the Agent settings layout", () => {
  const routes = [];
  const data = {
    id: "chores", name: "Chores", now: 0,
    setup: { kind: "ok", value: { project_id: "chores", team: { master: "codex", master_tools: true, sub_harnesses: [] } } },
    harnesses: undefined,
    assigned: { pool: { id: "pool-a", name: "Pool A", members: ["one", "two"] } },
    team: { result: { kind: "ok", value: status() }, at: 0 },
    navigate: (route) => routes.push(route), notify() {}, reload() {}, reloadPools() {}, reloadTeam() {},
  };
  assert.equal(typeof conversation.MasterSettings, "function", "conversation.tsx has MasterSettings");
  const html = renderToStaticMarkup(createElement(conversation.MasterSettings, { data }));
  const found = groups(html);
  assert.deepEqual(found.map((group) => group.title), MASTER_GROUPS.map(([title]) => title));
  MASTER_GROUPS.forEach(([title, rows], index) => {
    for (const row of rows) assert.ok(found[index].body.includes(`data-setting="${attr(row)}"`), `${row} is under ${title}`);
  });
  assert.match(found[0].body, /Codex/);
  assert.match(found[0].body, /Pool A/);
  assert.match(found[3].body, />Change in Agent settings</);
});
