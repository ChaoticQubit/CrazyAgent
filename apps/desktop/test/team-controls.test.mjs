import assert from "node:assert/strict";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import "./load-source.mjs";

const controls = await import("../src/screens/team/controls.tsx");

const harness = (name, role, extra = {}) => ({ harness: name, role, agents_max: 2, tools: true, live: 0, delegation: null, delegation_label: null, runnable: true, ...extra });
const status = (extra = {}) => ({
  project_id: "chores", keep_warm: false, paused: false, min_agents: 1, max_agents: 3,
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
  assert.match(html, /<option value="">Any sub-agent<\/option><option value="codex">codex<\/option><option value="claude">claude<\/option>/);
  assert.match(html, /<button type="submit" class="btn primary small" disabled=""/);
  assert.match(html, /Skips the master\. The master sees the task and its result\./);
  const lone = renderToStaticMarkup(createElement(controls.TeamControls, { projectId: "solo", status: status({ min_agents: 0, harnesses: [harness("codex", "master")] }), reload() {} }));
  assert.doesNotMatch(lone, /Hand a task/);
  assert.match(lone, /This team has no sub-agents/);
  assert.match(lone, /Keep 0 sub-agents ready/);
  assert.match(lone, /aria-disabled="true"/);
});
