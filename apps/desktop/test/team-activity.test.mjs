import assert from "node:assert/strict";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import "./load-source.mjs";

const activity = await import("../src/screens/project/team-activity.tsx");

const node = (id, kind, extra = {}) => ({
  id, kind, parent: null, label: id, harness: "codex", status: null, status_label: null, account: null, agents: null, terminal: null, ...extra,
});
const edge = (task_id, to, state, text) => ({ from: "m", to, task_id, source: "master", text, state, reply: null });
const agent = (id, extra = {}) => ({
  id, role: "sub", label: `agent-${id}`, harness: "codex", tools: true, account: null, worktree: null, status: "working", status_label: "Working",
  task: null, turns: 1, problem: null, process_group: null, created_ms: 1_000, heartbeat_ms: 1_000, stopped_ms: null, ...extra,
});
const task = (id, agentId, extra = {}) => ({
  id, source: "master", harness: "codex", tools: true, prompt: `Task ${id}`, status: "running", status_label: "Running", agent: agentId, attempts: 1,
  result: null, problem: null, delivered: false, output_bytes: 0, created_ms: 2_000, started_ms: 3_000, finished_ms: null, ...extra,
});

const longTask = "Fix the onboarding run name collision so a reused project id starts both the graph and mapping steps";
const graph = {
  project_id: "p-1",
  nodes: [
    node("m", "master", { label: "Master", status: "working", terminal: "team-9" }),
    node("g-codex", "harness_group", { parent: "m", label: "codex", agents: 2 }),
    node("team-1", "agent", { parent: "g-codex", label: "Ada (@onboarding-fix)", status: "working", status_label: "Working", terminal: "team-1" }),
    node("team-2", "agent", { parent: "g-codex", label: "Grace (@graph-layout-redesign-for-the-desktop)", status: "idle", status_label: "Idle", terminal: "team-2", questions: 2 }),
    node("team-3", "agent", { parent: "g-codex", label: "Linus (@usage)", status: "stopped", status_label: "Stopped" }),
  ],
  edges: [edge(1, "team-1", "running", longTask), edge(2, "team-3", "done", "Write the usage page"), edge(3, "team-2", "sent", "Answer the question")],
};
const status = {
  project_id: "p-1", keep_warm: false, paused: false, min_agents: 0, max_agents: 4, harnesses: [], messages: [], plan: [],
  agents: [agent(1, { created_ms: 1_000 }), agent(2, { created_ms: 5_000 }), agent(3, { created_ms: 1_000, stopped_ms: 590_000, status: "stopped" })],
  tasks: [task(1, 1, { started_ms: 120_000 }), task(2, 3, { status: "done", finished_ms: 400_000 }), task(3, 2, { created_ms: 10_000, started_ms: null })],
};

test("rows list every sub-agent, questions first, then live ones, then the latest change", () => {
  const rows = activity.activityRows(graph, status);
  assert.deepEqual(rows.map((row) => row.node.id), ["team-2", "team-1", "team-3"]);
  assert.deepEqual(rows.map((row) => row.changed), [10_000, 120_000, 590_000]);
  assert.deepEqual(rows.map((row) => row.questions), [2, 0, 0]);
  assert.equal(rows[1].task, longTask);
  assert.equal(rows[2].task, "Done: Write the usage page");
  assert.deepEqual(activity.activityRows(graph, null).map((row) => row.changed), [null, null, null]);
  assert.deepEqual(activity.activityRows({ ...graph, nodes: graph.nodes.slice(0, 2) }, status), []);
});

test("names and task lines are clipped to one line with an ellipsis", () => {
  const rows = activity.activityRows(graph, status);
  assert.equal(rows[0].name, "Grace (@graph-layout-rede...");
  assert.ok(Array.from(rows[0].name).length <= activity.ACTIVITY_NAME_CHARS);
  assert.equal(rows[1].line, "Fix the onboarding run name col...");
  assert.ok(Array.from(rows[1].line).length <= activity.ACTIVITY_TASK_CHARS);
  assert.equal(rows[2].line, "Done: Write the usage page");
});

test("a row shows its dot, harness, age and question badge, and only a live agent opens its terminal", () => {
  const opened = [];
  const html = renderToStaticMarkup(createElement(activity.TeamActivityList, {
    rows: activity.activityRows(graph, status), now: 600_000, open: "team-1", harness: (id) => ({ codex: "Codex" })[id] ?? id, onOpen: (id) => opened.push(id),
  }));
  assert.match(html, /<ul class="ta-rows" aria-label="Sub-agents">/);
  assert.equal((html.match(/<li>/g) ?? []).length, 3);
  assert.match(html, /class="tg-dot tone-info"/);
  assert.match(html, /<span class="tg-badge question" aria-label="2 questions waiting">/);
  assert.equal((html.match(/tg-badge question/g) ?? []).length, 1);
  assert.match(html, /Codex, Idle/);
  assert.match(html, /9 min ago/);
  assert.match(html, /title="Fix the onboarding run name collision so a reused project id starts both the graph and mapping steps"/);
  assert.match(html, /class="ta-row open"[^>]*aria-pressed="true"/);
  assert.match(html, /aria-label="Linus \(@usage\), Codex, Stopped\. Done: Write the usage page\. Changed just now\."[^>]*aria-disabled="true"/);
  assert.doesNotMatch(html, /Linus[^"]*Opens its terminal/);
  assert.match(html, /Ada \(@onboarding-fix\), Codex, Working\. [^"]*Opens its terminal\./);
});

test("the open target is the agent's terminal, never a stopped agent", () => {
  const rows = activity.activityRows(graph, status);
  assert.equal(activity.openTarget(rows, "team-1")?.terminal, "team-1");
  assert.equal(activity.openTarget(rows, "team-3"), undefined);
  assert.equal(activity.openTarget(rows, "team-404"), undefined);
  assert.equal(activity.openTarget(rows, null), undefined);
});

test("with no sub-agents the strip says the master starts them", () => {
  const html = renderToStaticMarkup(createElement(activity.TeamActivityList, { rows: [], now: 0, open: null, onOpen() {} }));
  assert.match(html, /No sub-agents yet\. The master starts them when it delegates\./);
  assert.doesNotMatch(html, /ta-rows/);
});

test("the master view keeps its content and adds the strip beside it", () => {
  const data = {
    id: "p-1", name: "Harbor Notes", setup: undefined, research: undefined, context: undefined, pools: undefined, harnesses: undefined,
    assigned: null, now: 0, navigate() {}, notify() {}, reload() {}, reloadPools() {}, team: { result: undefined, at: 0 }, reloadTeam() {},
  };
  const html = renderToStaticMarkup(createElement(activity.TeamActivity, { data }, createElement("p", null, "Master conversation")));
  assert.match(html, /<div class="ta-split"><div class="ta-main"><p>Master conversation<\/p><\/div><aside class="ta-side" aria-label="Team activity">/);
  assert.match(html, /aria-busy="true"/);
});
