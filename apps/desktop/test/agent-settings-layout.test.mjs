import "./load-source.mjs";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";

const controls = await import("../src/screens/team/controls.tsx");
const client = await import("../src/engine-client.ts");
const conversation = await import("../src/screens/project/conversation.tsx");
const agents = await import("../src/screens/project/agents.tsx");
const shared = await import("../src/screens/project/shared.tsx");
const rows = await import("../src/screens/team/setup-rows.tsx").catch(() => ({}));

const GROUPS = [
  ["model", "Model and harness", ["Master harness", "Sub-agent harnesses", "Keep sub-agents ready", "On master harness switch", "Spread harnesses across accounts", "Max harnesses per project", "Max teammates per harness"]],
  ["access", "Access and merging", ["Agent access", "Under Full access, merging into main", "Ask me before merging into the master's branch", "Agent branches", "After you accept, the agent's branch restarts from main", "Master's branch", "Land on", "Sub-agent work lands on its lead's branch", "Refuse a change set when one new folder adds more than"]],
  ["tools", "Skills and tools", ["Skills per role", "MCP servers", "Workflow nudges", "Task-start reminder", "Counted tools", "Re-ask a sub-agent's empty reply"]],
  ["coord", "Coordination", ["Claude Code sub-agents", "Codex sub-agents", "rcli sub-agents", "Sub-agent depth", "Sub-agent questions go to", "Overlapping edits", "Terminals per window", "Catch up with main", "Finished agents", "Messages to Claude Code agents", "Claude Code terminal display", "Messages to Codex agents", "Messages to rcli agents"]],
  ["limits", "Limits", ["Each task may run", "Each agent session may run", "Stop an idle sub-agent after", "Background commands at task end", "Token budget"]],
];
const MASTER_GROUPS = [
  ["Master", ["Harness", "Account"]],
  ["Conversation", ["On master harness switch", "Apply a master harness change"]],
  ["Messages", ["Sub-agent questions go to", "Messages to Claude Code agents", "Re-ask a sub-agent's empty reply"]],
  ["Access", ["Full access"]],
];
const MERGE = "Under Full access, merging into main";
const RESTART = "After you accept, the agent's branch restarts from main";

const attr = (text) => text.replace(/&/g, "&amp;").replace(/"/g, "&quot;").replace(/'/g, "&#x27;");
const harness = (name, role, extra = {}) => ({ harness: name, role, agents_max: 2, tools: true, live: 0, delegation: null, delegation_label: null, runnable: true, ...extra });
const status = (extra = {}) => ({
  project_id: "chores", keep_warm: false, idle_stop_secs: 600, paused: false, min_agents: 1, max_agents: 3,
  harnesses: [harness("codex", "master"), harness("codex", "sub"), harness("claude", "sub")],
  agents: [], tasks: [], messages: [], plan: [], ...extra,
});
const entryOf = (id, name, extra = {}) => ({ id, name, status: { kind: "ready" }, status_label: "Ready", executable: null, headless: "", tools: { kind: "switchable" }, model_selection: false, account_kinds: [], team: { kind: "available" }, aliases: [], ...extra });
const HARNESSES = [entryOf("claude", "Claude Code", { aliases: ["claude-code"] }), entryOf("codex", "Codex")];
const POOLS = [{ pool: { id: "pool-a", name: "Pool A", members: [] }, members: [] }, { pool: { id: "pool-b", name: "Pool B", members: [] }, members: [] }];
const SETUP = {
  project_id: "chores", name: "Chores", about: "", tech_stack: [], platforms: [], research: [], pool: "pool-a", revision: 4,
  team: { master: "codex", master_tools: true, sub_harnesses: [{ harness: "codex", agents: 2, tools: true }, { harness: "claude", agents: 3, tools: false }], min_agents: 1, max_agents: 6 },
};
const project = (extra = {}) => ({
  id: "chores", name: "Chores", now: 0, setup: { kind: "ok", value: SETUP }, harnesses: { kind: "ok", value: HARNESSES }, pools: { kind: "ok", value: POOLS },
  research: undefined, assigned: POOLS[0], team: { result: { kind: "ok", value: status() }, at: 0 },
  navigate() {}, notify() {}, reload() {}, reloadPools() {}, reloadTeam() {}, ...extra,
});
const settingsHtml = (extra = {}) => renderToStaticMarkup(createElement(controls.TeamControls, { projectId: "chores", status: status(extra), reload() {}, data: project() }));
const plain = (html) => html.replace(/<[^>]+>/g, " ").replace(/&#x27;/g, "'").replace(/&quot;/g, '"').replace(/&amp;/g, "&").replace(/\s+/g, " ").trim();

/** Cuts one setting row out of rendered settings. Inputs: the markup and the row's label.
 * Output: the markup from that row up to the next row. */
function row(html, label) {
  const start = html.indexOf(`data-setting="${attr(label)}"`);
  assert.ok(start >= 0, `a row named ${label}`);
  const next = html.indexOf('data-setting="', start + 1);
  return html.slice(start, next < 0 ? undefined : next);
}

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
  assert.deepEqual(controls.matchingGroups("  MAIN ").map((group) => [group.id, group.rows]), [["access", [MERGE, RESTART]], ["coord", ["Catch up with main"]]]);
  assert.deepEqual(controls.matchingGroups("harness").map((group) => [group.id, group.rows]),
    [["model", ["Master harness", "Sub-agent harnesses", "On master harness switch", "Spread harnesses across accounts", "Max harnesses per project", "Max teammates per harness"]]]);
  assert.deepEqual(controls.matchingGroups("mcp").map((group) => [group.id, group.rows]), [["tools", ["MCP servers"]]]);
  assert.deepEqual(controls.matchingGroups("").map((group) => group.rows), GROUPS.map(([, , rows]) => rows));
  assert.deepEqual(controls.matchingGroups("no such setting"), []);
});

test("Overlapping edits lists the engine's choices in the mockup's order and words and shows an unknown engine value as it is", () => {
  assert.deepEqual(client.OVERLAP_CHOICES, ["warn", "block", "off", "wait"]);
  const coordination = (overlaps) => ({ settings: { questions_to: "lead", overlaps, nudges: "off", counted_tools: [], role_skills: { master: null, lead: null, worker: null }, claude_display: "classic", task_start: "on" }, harnesses: [] });
  const words = ["Warn both (default)", "Wait until the first lands", "Block the later one", "Off"];
  assert.deepEqual(radios(settingsHtml({ coordination: coordination("block") }), "Overlapping edits"), { labels: words, checked: ["Block the later one"] });
  assert.deepEqual(radios(settingsHtml({ coordination: coordination("hold") }), "Overlapping edits"), { labels: [...words, "hold"], checked: ["hold"] });
});

test("a harness's sub-agents read as the mockup words them, native agent teams first where the harness has them", async () => {
  const coord = await import("../src/screens/team/coordination.tsx");
  assert.deepEqual(coord.modeOptions(true), [{ value: "native", label: "Native agent teams" }, { value: "app_spawned", label: "CrazyAgent workers" }]);
  assert.deepEqual(coord.modeOptions(false), [{ value: "app_spawned", label: "CrazyAgent workers" }]);
  const css = readFileSync(new URL("../src/screens/project/projects.css", import.meta.url), "utf8");
  const sub = /(?:^|\n)\.set-sub \{([^}]*)\}/.exec(css)?.[1] ?? "";
  assert.ok(sub.length > 0, "the sub-heading has its rule");
  assert.doesNotMatch(sub, /text-transform: uppercase/, "Branches reads as the mockup writes it, not in capitals");
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
  const data = project({ navigate: (route) => routes.push(route) });
  assert.equal(typeof conversation.MasterSettings, "function", "conversation.tsx has MasterSettings");
  const html = renderToStaticMarkup(createElement(conversation.MasterSettings, { data }));
  const found = groups(html);
  assert.deepEqual(found.map((group) => group.title), MASTER_GROUPS.map(([title]) => title));
  MASTER_GROUPS.forEach(([title, rows], index) => {
    for (const row of rows) assert.ok(found[index].body.includes(`data-setting="${attr(row)}"`), `${row} is under ${title}`);
  });
  assert.match(row(html, "Harness"), /<select[^>]*>[\s\S]*<option value="codex" selected="">Codex<\/option>/, "the master's harness is a select at the saved harness");
  assert.match(row(html, "Harness"), /<option value="claude"[^>]*>Claude Code<\/option>/);
  const account = row(html, "Account");
  assert.match(account, /<select[^>]*>[\s\S]*<option value="pool-a" selected="">Any in Pool A<\/option>/, "the account is a select of the project's pools");
  assert.match(account, /<option value="pool-b"[^>]*>Any in Pool B<\/option>/);
  assert.match(plain(account), /From the project's pool; fails over to the next account\./);
  assert.doesNotMatch(found[0].body, />(Change master|Change pool|Choose pool)</, "no button leaves the drawer to pick the harness or account");
  assert.doesNotMatch(html, /Stop the master when idle after/, "no idle-stop row while it has no setting");
  assert.match(found[3].body, />Change in Agent settings</);
});

test("Model and harness picks the master harness and each sub-agent harness's count and tools in place, from the saved team", () => {
  const html = settingsHtml();
  const master = row(html, "Master harness");
  assert.match(master, /<select[^>]*>[\s\S]*<option value="codex" selected="">Codex<\/option>/, "a Master harness select at the saved harness");
  assert.match(plain(master), /The master plans, hands out tasks and merges\./);
  const subs = row(html, "Sub-agent harnesses");
  assert.match(subs, /role="table" aria-label="Sub-agent harnesses"/);
  assert.deepEqual([...subs.matchAll(/role="columnheader"[^>]*>([^<]*)</g)].map((head) => head[1]), ["Harness", "Agents", "Tools"]);
  assert.deepEqual([...subs.matchAll(/<output[^>]*>(\d+)<\/output>/g)].map((count) => Number(count[1])), [2, 3], "each harness's agents at once");
  assert.deepEqual([...subs.matchAll(/aria-label="(Fewer|More) ([^"]+) agents"/g)].map((step) => `${step[1]} ${step[2]}`), ["Fewer Codex", "More Codex", "Fewer Claude Code", "More Claude Code"]);
  assert.deepEqual([...subs.matchAll(/role="switch"[^>]*aria-checked="(true|false)"/g)].map((tools) => tools[1]), ["true", "false"], "each harness's Tools switch");
  assert.match(plain(subs), /Tools for Codex[\s\S]*Tools for Claude Code/);
  assert.match(plain(subs), /How many agents each harness may run at once\./);
  assert.doesNotMatch(html, /Change in Settings, Team/, "the team is changed in place");
});

test("Access and merging words the merge choice as the mockup does, with its two cards, and the restart line as Always", () => {
  const cards = (html) => [...row(html, MERGE).matchAll(/<label class="radio-card">([\s\S]*?)<\/label>/g)].map((card) => ({ checked: /checked=""/.test(card[1]), text: plain(card[1]) }));
  const merges = settingsHtml({ master_lands: "merges" });
  assert.deepEqual(cards(merges), [
    { checked: true, text: "Master resolves conflicts and merges into main itself Default Finished work lands on main as soon as the master has resolved it and its checks pass." },
    { checked: false, text: "Master resolves conflicts, then waits for my accept The resolved change set waits in Changes until you accept it." },
  ]);
  assert.deepEqual(cards(settingsHtml({ master_lands: "waits" })).map((card) => card.checked), [false, true]);
  assert.match(plain(row(merges, MERGE)), /Each merge shows in Activity and can be undone\. Sandboxed keeps today's limits\./);
  assert.match(plain(row(settingsHtml(), MERGE)), /Only at Full access\./, "below Full access the engine sends no merge choice");
  assert.deepEqual(cards(settingsHtml()).map((card) => card.text), cards(merges).map((card) => card.text), "below Full access the row shows the same two choices");
  assert.equal([...row(settingsHtml(), MERGE).matchAll(/<input type="radio"[^>]*disabled=""/g)].length, 2, "below Full access both choices are locked");
  const restart = row(merges, RESTART);
  assert.match(plain(restart), /Always/);
  assert.doesNotMatch(restart, /<(button|select|input|textarea)/, "the restart line has no control while it has no setting");
  assert.doesNotMatch(merges, /Change in Settings, Agent access/, "agent access is changed in place");
});

test("the MCP servers row lists the servers this project's agents can reach", () => {
  assert.equal(typeof rows.reachableServers, "function", "setup-rows.tsx has reachableServers");
  const server = (name, extra = {}) => ({ id: name, name, scope: { kind: "global" }, scope_label: "", spec: { kind: "stdio", command: name, args: [] }, runs: "", enabled: true,
    status: "approved", status_label: "Approved", problem: null, tools: [], checks: 0, secrets: [], on_here: null, version: 1, ...extra });
  assert.deepEqual(rows.reachableServers([server("graphify"), server("context-mode", { on_here: true }), server("off", { enabled: false }), server("not-here", { on_here: false }),
    server("waiting", { status: "waiting_for_approval" }), server("broken", { status: "cant_start" })]), ["graphify", "context-mode"]);
  assert.deepEqual(rows.reachableServers([]), []);
});

test("a moved Agent access choice says before Save that a lowering stops agents above the new level", () => {
  assert.equal(typeof rows.accessChangeLine, "function", "setup-rows.tsx has accessChangeLine");
  assert.equal(rows.accessChangeLine("full", "sandboxed"), "Saving lowers agent access from Full access to Sandboxed: agents running above Sandboxed stop and restart at it.");
  assert.equal(rows.accessChangeLine("own_branch", "sandboxed"), "Saving lowers agent access from Own branch to Sandboxed: agents running above Sandboxed stop and restart at it.");
  assert.equal(rows.accessChangeLine("sandboxed", "full"), "Saving raises agent access from Sandboxed to Full access.");
  assert.throws(() => rows.accessChangeLine("full", "full"), /two different levels/);
});

test("the Account choices are the project's pools, any account in each, then no pool", () => {
  assert.equal(typeof rows.accountOptions, "function", "setup-rows.tsx has accountOptions");
  assert.deepEqual(rows.accountOptions(POOLS), [{ value: "pool-a", label: "Any in Pool A" }, { value: "pool-b", label: "Any in Pool B" }, { value: "", label: "No pool" }]);
  assert.deepEqual(rows.accountOptions([]), [{ value: "", label: "No pool" }]);
});

test("an agents?section= route opens Agent settings at the group it names", () => {
  assert.equal(typeof shared.routeSection, "function", "shared.tsx has routeSection");
  assert.equal(shared.routeSection("#/projects/alpha/agents?section=access"), "access");
  assert.equal(shared.routeSection("#/projects/alpha/agents?section=settings&x=1"), "settings");
  assert.equal(shared.routeSection("#/projects/alpha/agents"), undefined);
  assert.deepEqual(agents.agentsView(shared.routeSection("#/projects/alpha/agents?section=tools")), { view: "settings", group: "tools" });
});

test("Overlapping edits and the merge choice take the row's full width with the help above the choices, as the mockup draws them", () => {
  const html = settingsHtml({ master_lands: "merges" });
  for (const [label, help] of [["Overlapping edits", "Warn tells both agents"], [MERGE, "Each merge shows in Activity"]]) {
    const start = html.indexOf(`data-setting="${attr(label)}"`);
    assert.match(html.slice(html.lastIndexOf("<div", start), start), /class="set-row wide"/, `${label} takes the row's full width`);
    const part = row(html, label);
    const [words, choices] = [part.indexOf(help), part.indexOf(`aria-label="${attr(label)}"`)];
    assert.ok(words >= 0 && choices >= 0, `${label} has its help and its choices`);
    assert.ok(words < choices, `${label}: the help sits above the choices`);
  }
});
