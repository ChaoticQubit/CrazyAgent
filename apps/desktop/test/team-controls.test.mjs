import assert from "node:assert/strict";
import test from "node:test";
import { readFileSync, readdirSync } from "node:fs";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import "./load-source.mjs";

const controls = await import("../src/screens/team/controls.tsx");

const picked = (html, id) => {
  const select = html.match(new RegExp(`<select[^>]*id="${id}"[^>]*>([\\s\\S]*?)</select>`));
  assert.ok(select, id);
  return [...select[1].matchAll(/<option value="([^"]*)" selected="">/g)].map((found) => found[1]);
};
const harness = (name, role, extra = {}) => ({ harness: name, role, agents_max: 2, tools: true, live: 0, delegation: null, delegation_label: null, runnable: true, ...extra });
const status = (extra = {}) => ({
  project_id: "chores", keep_warm: false, idle_stop_secs: 600, paused: false, min_agents: 1, max_agents: 3,
  harnesses: [harness("codex", "master"), harness("codex", "sub"), harness("claude", "sub"), harness("codex", "sub", { tools: false })],
  agents: [], tasks: [], messages: [], plan: [], ...extra,
});

test("a task can go to any sub-agent or to one sub-agent harness of the plan, each once", () => {
  const name = (id) => ({ codex: "Codex", claude: "Claude Code" })[id] ?? id;
  assert.deepEqual(controls.delegateOptions(status(), name), [
    { value: "", label: "Any sub-agent" },
    { value: "codex", label: "Codex" },
    { value: "claude", label: "Claude Code" },
  ]);
  assert.deepEqual(controls.delegateOptions(status({ harnesses: [harness("codex", "master")] })), [{ value: "", label: "Any sub-agent" }]);
});

test("a task is checked with the engine's rules before it is handed off", () => {
  assert.deepEqual(controls.taskInput("  Write chores.md\n"), { kind: "ok", prompt: "Write chores.md" });
  assert.deepEqual(controls.taskInput("Line one\n\tLine two"), { kind: "ok", prompt: "Line one\n\tLine two" });
  assert.deepEqual(controls.taskInput("   "), { kind: "invalid", reason: "Write the task first." });
  assert.deepEqual(controls.taskInput("bell\u0007"), { kind: "invalid", reason: "Remove the control characters from the task." });
  assert.equal(controls.taskInput("é".repeat(2000)).kind, "ok");
  assert.deepEqual(controls.taskInput("é".repeat(2001)), { kind: "invalid", reason: "Too long: 4002 of 4000 bytes." });
});

test("a team change says nothing when applied and plain words otherwise", () => {
  const receipt = (extra) => ({ outcome: "applied", pending: false, rejection: null, ...extra });
  assert.equal(controls.changeNote({ kind: "ok", value: receipt({}) }, "x"), "");
  assert.equal(controls.changeNote({ kind: "not_ready" }, "handing tasks to sub-agents"), "Not ready yet: handing tasks to sub-agents isn't in this build.");
  assert.equal(controls.changeNote({ kind: "ok", value: receipt({ pending: true }) }, "x"), "We couldn't confirm your last change. Check the team before trying again.");
  assert.notEqual(controls.changeNote({ kind: "ok", value: receipt({ outcome: "rejected", rejection: "team_harness_not_in_plan" }) }, "x"), "");
  assert.notEqual(controls.changeNote({ kind: "error", code: "owner_unavailable", message: "m" }, "x"), "");
});

test("the controls show the keep-ready switch and the hand-off form for a team with sub-agents", () => {
  const html = renderToStaticMarkup(createElement(controls.TeamControls, { projectId: "chores", status: status({ keep_warm: true }), reload() {} }));
  assert.match(html, /aria-label="Team controls"/);
  assert.match(html, /role="switch" class="switch" aria-checked="true"/);
  assert.match(html, /Keep 1 sub-agent ready/);
  assert.match(html, /<h4>Hand a task to a sub-agent<\/h4>/);
  assert.match(html, /<option value="" selected="">Any sub-agent<\/option><option value="codex">codex<\/option><option value="claude">claude<\/option>/);
  assert.match(html, /<button type="submit" class="btn primary small" disabled=""/);
  assert.match(html, /Skips the master\. The master sees the task and its result\./);
  const lone = renderToStaticMarkup(createElement(controls.TeamControls, { projectId: "solo", status: status({ min_agents: 0, harnesses: [harness("codex", "master")] }), reload() {} }));
  assert.doesNotMatch(lone, /Hand a task/);
  assert.match(lone, /This team has no sub-agents/);
  assert.match(lone, /Keep 0 sub-agents ready/);
  assert.match(lone, /aria-disabled="true"/);
});

test("each time limit offers Off and 30 min to 8 h, with Off the default", () => {
  assert.deepEqual(controls.limitOptions(null).map((option) => [option.value, option.label]), [
    ["off", "Off"], ["1800", "30 min"], ["3600", "1 h"], ["7200", "2 h"], ["14400", "4 h"], ["28800", "8 h"],
  ]);
  assert.deepEqual(controls.limitOptions(90).at(-1), { value: "90", label: "90 s" }, "a stored value outside the choices still shows");
  const html = renderToStaticMarkup(createElement(controls.TeamControls, { projectId: "chores", status: status(), reload() {} }));
  assert.match(html, /<label for="tg-turn-limit-chores">Each task may run<\/label>/);
  assert.match(html, /<label for="tg-session-limit-chores">Each agent session may run<\/label>/);
  for (const id of ["tg-turn-limit-chores", "tg-session-limit-chores"]) {
    assert.deepEqual(picked(html, id), ["off"], `${id}: both limits are Off until the owner sets one`);
  }
  const set = renderToStaticMarkup(createElement(controls.TeamControls, { projectId: "chores", status: status({ turn_limit_secs: 3600, session_limit_secs: 14_400 }), reload() {} }));
  assert.match(set, /<option value="3600" selected="">1 h<\/option>/);
  assert.match(set, /<option value="14400" selected="">4 h<\/option>/);
});

test("the idle stop offers Off, 2 min, 10 min and 1 h, and shows the engine's 10 min default", () => {
  assert.deepEqual(controls.limitOptions(null, controls.IDLE_CHOICES_SECS).map((option) => [option.value, option.label]), [
    ["off", "Off"], ["120", "2 min"], ["600", "10 min"], ["3600", "1 h"],
  ]);
  const html = renderToStaticMarkup(createElement(controls.TeamControls, { projectId: "chores", status: status({ idle_stop_secs: 600 }), reload() {} }));
  assert.match(html, /<label for="tg-idle-stop-chores">Stop an idle sub-agent after<\/label>/);
  assert.match(html, /<option value="600" selected="">10 min<\/option>/);
  const off = renderToStaticMarkup(createElement(controls.TeamControls, { projectId: "chores", status: status({ idle_stop_secs: null }), reload() {} }));
  for (const id of ["tg-idle-stop-chores", "tg-turn-limit-chores", "tg-session-limit-chores"]) {
    assert.deepEqual(picked(off, id), ["off"], `${id}: the idle stop Off with both time limits Off`);
  }
});

test("a changed limit keeps the session limit not below the task limit", () => {
  assert.deepEqual(controls.limitPair(null, null, "turn", 3600), { turn: 3600, session: null });
  assert.deepEqual(controls.limitPair(3600, 7200, "turn", 14_400), { turn: 14_400, session: 14_400 });
  assert.deepEqual(controls.limitPair(3600, 7200, "session", 1800), { turn: 1800, session: 1800 });
  assert.deepEqual(controls.limitPair(null, null, "session", 7200), { turn: null, session: 7200 }, "a session limit needs no task limit");
  assert.deepEqual(controls.limitPair(3600, 7200, "turn", null), { turn: null, session: 7200 }, "a task limit turned Off leaves the session limit");
  assert.deepEqual(controls.limitPair(3600, 7200, "session", null), { turn: 3600, session: null });
});

test("the On master harness switch setting offers Carry over first and shows the engine's choice", () => {
  assert.deepEqual(controls.SWITCH_OPTIONS.map((option) => option.value), ["carry", "fresh"]);
  assert.deepEqual(controls.SWITCH_OPTIONS.map((option) => option.label), ["Carry conversation over", "Start fresh"]);
  assert.equal(controls.switchMode(status()), "carry");
  assert.equal(controls.switchMode(status({ on_harness_switch: "fresh" })), "fresh");
  const carry = renderToStaticMarkup(createElement(controls.TeamControls, { projectId: "chores", status: status(), reload() {} }));
  assert.match(carry, /<label for="tg-switch-chores">On master harness switch<\/label>/);
  assert.match(carry, /<option value="carry" selected="">Carry conversation over<\/option><option value="fresh">Start fresh<\/option>/);
  const fresh = renderToStaticMarkup(createElement(controls.TeamControls, { projectId: "chores", status: status({ on_harness_switch: "fresh" }), reload() {} }));
  assert.match(fresh, /<option value="carry">Carry conversation over<\/option><option value="fresh" selected="">Start fresh<\/option>/);
});

test("Land on offers the master's branch by default and a named branch, and shows the engine's choice", () => {
  const render = (extra) => renderToStaticMarkup(createElement(controls.TeamControls, { projectId: "chores", status: status(extra), reload() {} }));
  const field = (value) => `<input id="tg-land-on-chores" class="pj-input" placeholder="The master&#x27;s branch" aria-describedby="tg-land-on-help-chores" value="${value}"/>`;
  const unset = render();
  assert.match(unset, /<form class="pj-row" aria-label="Land on"><label for="tg-land-on-chores">Land on<\/label>/);
  assert.ok(unset.includes(field("")), "no branch named shows the master's branch as the default");
  assert.ok(unset.includes('<button type="button" class="btn small" disabled="">Use the master&#x27;s branch</button>'), "with no branch named there is nothing to clear");
  const named = render({ land_on: "stage" });
  assert.ok(named.includes(field("stage")), "a named branch shows as the engine keeps it");
  assert.ok(named.includes('<button type="button" class="btn small">Use the master&#x27;s branch</button>'), "a named branch can go back to the master's branch");
});

test("Background commands at task end offers Wait first, a grace time and Stop, says which agents it acts on, and shows the engine's choice", () => {
  const render = (extra) => renderToStaticMarkup(createElement(controls.TeamControls, { projectId: "chores", status: status(extra), reload() {} }));
  const wait = render();
  assert.match(wait, /<label for="tg-background-chores">Background commands at task end<\/label>/);
  assert.match(wait, /<option value="wait" selected="">Wait for them<\/option><option value="grace">Stop them after a grace time<\/option><option value="stop">Stop them when the turn ends<\/option>/);
  assert.doesNotMatch(wait, /id="tg-grace-chores"/, "no grace time to type while Wait is on");
  assert.ok(wait.includes("Wait and a grace time act on Claude Code panes. Other agents run per turn have their background commands stopped when the turn ends, and other kept agents, such as a kept Codex pane, keep theirs whatever the choice."), "the setting says which agents it acts on");
  const grace = render({ background_at_end: { mode: "grace", secs: 300 } });
  assert.match(grace, /<option value="wait">Wait for them<\/option><option value="grace" selected="">Stop them after a grace time<\/option>/);
  assert.match(grace, /<label for="tg-grace-chores">Grace time in seconds<\/label><input id="tg-grace-chores"[^>]* value="300"/);
  assert.ok(grace.includes("From 10 to 3600 seconds."), "the grace time says its range");
  const stop = render({ background_at_end: { mode: "stop" } });
  assert.match(stop, /<option value="grace">Stop them after a grace time<\/option><option value="stop" selected="">Stop them when the turn ends<\/option>/);
  assert.doesNotMatch(stop, /id="tg-grace-chores"/, "no grace time to type while Stop is on");
  assert.deepEqual(["10", "3600", "120", " 45 "].map(controls.graceSecs), [10, 3600, 120, 45]);
  assert.deepEqual(["9", "3601", "1.5", "", "2e3", "-10", "0120x"].map(controls.graceSecs), [null, null, null, null, null, null, null]);
  assert.deepEqual(controls.backgroundChoice(status()), { mode: "wait" });
});

test("a team view the engine shortened to fit says older history is hidden", () => {
  const whole = renderToStaticMarkup(createElement(controls.TeamControls, { projectId: "chores", status: status(), reload() {} }));
  assert.doesNotMatch(whole, /Older team history is hidden/);
  const partial = renderToStaticMarkup(createElement(controls.TeamControls, { projectId: "chores", status: status({ partial: true }), reload() {} }));
  assert.match(partial, /<p class="pj-note" role="status">Older team history is hidden so the team fits in one view\. Queued and running work stays listed\.<\/p>/);
});

test("the idle stop says when an idle master stops, which no setting changes", () => {
  const html = renderToStaticMarkup(createElement(controls.TeamControls, { projectId: "chores", status: status(), reload() {} }));
  assert.match(html, /An idle master stops 15 min after its work is done and is never stopped while that work is still going\./);
});

test("two saves from one view both name the version that view read, so the second is refused instead of undoing the first", () => {
  assert.equal(controls.settingsVersion(status()), 0, "an engine without the field names 0");
  const view = status({ settings_version: 3, turn_limit_secs: 3600, session_limit_secs: null, idle_stop_secs: null });
  assert.equal(controls.settingsVersion(view), 3, "the first save, a 2 h task limit, names the read");
  assert.equal(controls.settingsVersion(view), 3, "the second save, an idle stop built from the same view's 1 h task limit before the team read returns, names the same read, which the engine now refuses as stale");
  const source = readFileSync(new URL("../src/screens/team/controls.tsx", import.meta.url), "utf8");
  assert.doesNotMatch(source, /setSaved|Math\.max/, "no version from an earlier save is ever named with values the view did not read");
  assert.match(source, /const saved: Limits = \{ turn: status\.turn_limit_secs \?\? null, session: status\.session_limit_secs \?\? null, idle: status\.idle_stop_secs \?\? null \};/, "the limits a change starts from are read from the shown status");
  assert.match(source, /const shown = shownChoice\(limits, saved, sameLimits\);/, "a held change shows only while the status still holds what it replaced");
  assert.match(source, /const result = await send\(settingsVersion\(status\)\);/, "every save names the version of the status the view read");
});

test("every native flow that changes a team setting names the settings version it read", () => {
  const folder = new URL("./native/", import.meta.url);
  let calls = 0;
  for (const file of readdirSync(folder).filter((name) => name.endsWith(".mjs"))) {
    const source = readFileSync(new URL(file, folder), "utf8");
    for (const call of source.matchAll(/"desktop_team_(?:keep_warm|time_limits|on_harness_switch|finished_agents|live_master)", \{[^}]*\}/g)) {
      calls += 1;
      assert.match(call[0], /\bversion: /, `${file}: ${call[0]}`);
    }
  }
  assert.ok(calls > 0, "the native flows change a team setting");
});

test("each team setting sends the version it read, and a stale save says so and reads the team again", () => {
  const source = readFileSync(new URL("../src/screens/team/controls.tsx", import.meta.url), "utf8");
  for (const call of [
    "keepTeamWarm(projectId, on, version)",
    "setTeamTimeLimits(projectId, next.turn, next.session, next.idle, version)",
    "setOnHarnessSwitch(projectId, mode, version)",
    'showFinishedAgents(projectId, choice === "show", version)',
  ]) assert.ok(source.includes(`await save((version) => ${call}`), call);
  assert.match(source, /const result = await send\(settingsVersion\(status\)\);\s*return savedStale\(result\) \? TEAM_SETTINGS_STALE : changeNote\(result, missing\);/);
  for (const handler of ["toggle", "limit", "chooseSwitch", "view"]) {
    const body = source.slice(source.indexOf(`async function ${handler}(`), source.indexOf("\n  }\n", source.indexOf(`async function ${handler}(`)));
    assert.match(body, /setNote\(message\);\s*reload\(\);$/, `${handler} reads the team again after every save, stale or not`);
  }
  assert.equal(controls.TEAM_SETTINGS_STALE, "Team settings changed since the controls read them, so your change was not saved. The controls now show the saved settings; make your change again.");
  const api = readFileSync(new URL("../src/api.ts", import.meta.url), "utf8");
  const client = readFileSync(new URL("../src/engine-client.ts", import.meta.url), "utf8");
  for (const [command, fields] of [
    ["desktop_team_keep_warm", "id, on, version"],
    ["desktop_team_time_limits", "id, turnSecs, sessionSecs, idleSecs, version"],
    ["desktop_team_on_harness_switch", "id, mode, version"],
    ["desktop_team_apply_harness_switch", "id, mode, version"],
    ["desktop_team_finished_agents", "id, show, version"],
    ["desktop_team_live_master", "id, on, version"],
  ]) {
    assert.ok(api.includes(`call<IntentReceipt>("${command}", { ${fields}, generation: nextGeneration() })`), `${command} in api.ts`);
    assert.ok(client.includes(`invoke<IntentReceipt>("${command}", { ${fields}, generation })`), `${command} in engine-client.ts`);
  }
});

const coordination = await import("../src/screens/team/coordination.tsx");

test("workflow nudges offer Off first, Every 10 minutes and First edit, and show Off for a project with no saved coordination settings", () => {
  assert.equal(coordination.COORD_DEFAULTS.nudges, "off");
  const html = renderToStaticMarkup(createElement(coordination.CoordinationControls, { projectId: "chores", status: status(), reload: () => {} }));
  const group = html.match(/<div class="segmented" role="radiogroup" aria-label="Workflow nudges">(.*?)<\/div>/)[1];
  const choices = [...group.matchAll(/aria-checked="(true|false)"[^>]*>([^<]+)<\/button>/g)].map(([, checked, label]) => [label, checked === "true"]);
  assert.deepEqual(choices, [["Off", true], ["Every 10 minutes", false], ["First edit", false]], "the default comes first, as in the panel's other settings");
});

const rustList = (name) => {
  const source = readFileSync(new URL("../../../crates/crazyagent/src/team.rs", import.meta.url), "utf8");
  const found = source.match(new RegExp(`pub const ${name}: \\[u\\d+; \\d+\\] = \\[([^\\]]*)\\]`));
  assert.ok(found, name);
  return found[1].split(",").map((item) => item.trim().replaceAll("_", "")).filter(Boolean).map(Number);
};

test("every cap, token budget, depth and landing choice is offered once, from the engine's own lists, and reads back", () => {
  assert.deepEqual([...controls.CAP_CHOICES], rustList("CAP_CHOICES"));
  assert.deepEqual([...controls.BUDGET_CHOICES], rustList("BUDGET_CHOICES"));
  assert.deepEqual(rustList("DEPTH_CHOICES"), controls.DEPTH_OPTIONS.map((option) => Number(option.value)));
  const caps = controls.capOptions();
  assert.deepEqual(caps.map((option) => [option.value, option.label]), [["off", "No limit"], ...controls.CAP_CHOICES.map((cap) => [String(cap), String(cap)])]);
  for (const option of caps) {
    assert.equal(controls.chosen(option.value, controls.CAP_CHOICES), option.value === "off" ? null : Number(option.value));
  }
  const budgets = controls.budgetOptions();
  assert.deepEqual(budgets.map((option) => option.label), ["Off", "10M tokens", "25M tokens", "50M tokens", "100M tokens", "250M tokens", "500M tokens", "1000M tokens"]);
  for (const option of budgets) {
    assert.equal(controls.chosen(option.value, controls.BUDGET_CHOICES), option.value === "off" ? null : Number(option.value));
  }
  assert.equal(controls.chosen("7", controls.CAP_CHOICES), undefined, "a value off the list is not a choice");
  assert.equal(controls.chosen("12000000", controls.BUDGET_CHOICES), undefined);
  assert.deepEqual([999_999, 1_000_000, 12_500_000, 12_590_000].map(controls.tokenWords), ["999999 tokens", "1M tokens", "12.5M tokens", "12.5M tokens"]);
  assert.equal(controls.DEPTH_DEFAULT, 2);
  assert.deepEqual(controls.LANDING_OPTIONS.map((option) => option.value), ["lead", "owner"]);
});

test("team settings changes send the version the window read, so the engine refuses a stale one", () => {
  const read = (path) => readFileSync(new URL(path, import.meta.url), "utf8");
  const client = read("../src/engine-client.ts");
  assert.match(client, /settings_version\?: number;/);
  assert.doesNotMatch(client, /settings_revision/, "one settings version, no second revision");
  const lib = read("../src-tauri/src/lib.rs");
  for (const command of ["desktop_team_time_limits", "desktop_team_on_harness_switch", "desktop_team_apply_harness_switch"]) {
    const start = lib.indexOf(`fn ${command}(`);
    assert.ok(start >= 0, command);
    assert.ok(lib.slice(start, lib.indexOf(")", start)).includes("version: u64"), `${command} takes the version`);
  }
  const limits = lib.indexOf("CommandArg<'de, R> for TeamTimeLimits");
  assert.ok(limits >= 0, "the native command reads the time limits through one argument");
  const reads = lib.slice(limits, lib.indexOf("\n}\n", limits));
  for (const key of ["turnSecs", "sessionSecs", "idleSecs"]) {
    assert.ok(reads.includes(`read("${key}")`), `the native command reads ${key} as its own key`);
  }
});

test("Settings, Team offers Apply a master harness change with When the master is idle by default and At the next start (N-D26)", async () => {
  const settings = await import("../src/screens/project/settings.tsx");
  assert.ok(Array.isArray(settings.APPLY_OPTIONS), "Settings, Team names the choices for a saved master harness change");
  assert.deepEqual(settings.APPLY_OPTIONS.map((option) => [option.value, option.label]), [["when_idle", "When the master is idle"], ["next_start", "At the next start"]]);
  assert.equal(settings.applyMode(status()), "when_idle", "an engine that sends no choice reads the default");
  assert.equal(settings.applyMode(status({ apply_harness_switch: "next_start" })), "next_start");
  const data = (value) => ({ id: "chores", team: { result: { kind: "ok", value }, at: 1 }, reloadTeam() {} });
  const idle = renderToStaticMarkup(createElement(settings.ApplySwitch, { data: data(status()) }));
  assert.match(idle, /Apply a master harness change/);
  assert.deepEqual(picked(idle, "ps-apply-switch"), ["when_idle"]);
  const next = renderToStaticMarkup(createElement(settings.ApplySwitch, { data: data(status({ apply_harness_switch: "next_start" })) }));
  assert.deepEqual(picked(next, "ps-apply-switch"), ["next_start"]);
  assert.equal(renderToStaticMarkup(createElement(settings.ApplySwitch, { data: { id: "chores", team: { result: undefined, at: 0 }, reloadTeam() {} } })), "", "nothing before the team status is read");
  const source = readFileSync(new URL("../src/screens/project/settings.tsx", import.meta.url), "utf8");
  const choose = source.slice(source.indexOf("export function ApplySwitch("), source.indexOf("\n  return <>", source.indexOf("export function ApplySwitch(")));
  assert.match(choose, /const result = await setApplyHarnessSwitch\(data\.id, mode, settingsVersion\(status\)\);/, "the change names the team settings version the page read (I1)");
  assert.match(choose, /const message = savedStale\(result\) \? TEAM_SETTINGS_STALE : changeNote\(result, "the Apply a master harness change setting"\);/, "a change from an older read says the settings changed and shows the saved ones");
  assert.match(choose, /setNote\(message\);\s*data\.reloadTeam\(\);/, "the page reads the team again after every save, stale or not");
});
