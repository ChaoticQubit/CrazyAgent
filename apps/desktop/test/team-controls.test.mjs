import assert from "node:assert/strict";
import test from "node:test";
import { readFileSync } from "node:fs";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import "./load-source.mjs";

const controls = await import("../src/screens/team/controls.tsx");

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
  assert.equal(html.match(/<option value="off" selected="">Off<\/option>/g)?.length, 2, "both limits are Off until the owner sets one");
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
  assert.equal(off.match(/<option value="off" selected="">Off<\/option>/g)?.length, 3, "the idle stop Off with both time limits Off");
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

test("a team setting change names the newer of the team read and this screen's own last save", () => {
  assert.equal(controls.settingsVersion(status(), null), 0, "an engine without the field names 0");
  assert.equal(controls.settingsVersion(status({ settings_version: 4 }), null), 4);
  assert.equal(controls.settingsVersion(status({ settings_version: 4 }), { project: "chores", version: 5 }), 5, "a second change before the team read returns is not stale");
  assert.equal(controls.settingsVersion(status({ settings_version: 6 }), { project: "chores", version: 5 }), 6, "a newer read wins over an older save");
  assert.equal(controls.settingsVersion(status({ settings_version: 2 }), { project: "other", version: 9 }), 2, "another project's save is not this team's version");
});

test("each team setting sends the version it read, and a stale save says so and reads the team again", () => {
  const source = readFileSync(new URL("../src/screens/team/controls.tsx", import.meta.url), "utf8");
  for (const call of [
    "keepTeamWarm(projectId, on, version)",
    "setTeamTimeLimits(projectId, next.turn, next.session, next.idle, version)",
    "setOnHarnessSwitch(projectId, mode, version)",
    'showFinishedAgents(projectId, choice === "show", version)',
  ]) assert.ok(source.includes(`await save((version) => ${call}`), call);
  assert.match(source, /const version = settingsVersion\(status, saved\);\s*const result = await send\(version\);\s*if \(savedStale\(result\)\) return TEAM_SETTINGS_STALE;\s*const message = changeNote\(result, missing\);\s*if \(!message\) setSaved\(\{ project: status\.project_id, version: version \+ 1 \}\);/);
  for (const handler of ["toggle", "limit", "chooseSwitch", "view"]) {
    const body = source.slice(source.indexOf(`async function ${handler}(`), source.indexOf("\n  }\n", source.indexOf(`async function ${handler}(`)));
    assert.match(body, /setNote\(message\);\s*reload\(\);$/, `${handler} reads the team again after every save, stale or not`);
  }
  assert.equal(controls.TEAM_SETTINGS_STALE, "Team settings changed in another window, so your change was not saved. The controls now show the saved settings; make your change again.");
  const api = readFileSync(new URL("../src/api.ts", import.meta.url), "utf8");
  const client = readFileSync(new URL("../src/engine-client.ts", import.meta.url), "utf8");
  for (const [command, fields] of [
    ["desktop_team_keep_warm", "id, on, version"],
    ["desktop_team_time_limits", "id, turnSecs, sessionSecs, idleSecs, version"],
    ["desktop_team_on_harness_switch", "id, mode, version"],
    ["desktop_team_finished_agents", "id, show, version"],
    ["desktop_team_live_master", "id, on, version"],
  ]) {
    assert.ok(api.includes(`call<IntentReceipt>("${command}", { ${fields}, generation: nextGeneration() })`), `${command} in api.ts`);
    assert.ok(client.includes(`invoke<IntentReceipt>("${command}", { ${fields}, generation })`), `${command} in engine-client.ts`);
  }
});
