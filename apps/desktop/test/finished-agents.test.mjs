import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import "./load-source.mjs";

const research = await import("../src/screens/project/research.ts");
const team = await import("../src/screens/project/team.tsx");
const controls = await import("../src/screens/team/controls.tsx");

const MINUTE = 60_000;
const agent = (id, status, extra = {}) => ({ id, role: "sub", label: `Sub-agent ${id}`, harness: "codex", tools: true, account: null, worktree: null, status, status_label: "",
  task: null, turns: 0, problem: null, process_group: null, created_ms: 0, heartbeat_ms: 0, stopped_ms: null, finished: false, register: null, ...extra });
const task = (id, status, extra = {}) => ({ id, source: "master", harness: "codex", tools: true, prompt: `Task ${id}`, status, status_label: "", agent: null, attempts: 1,
  result: null, problem: null, delivered: false, output_bytes: 0, created_ms: 0, started_ms: null, finished_ms: null, ...extra });

function teamStatus(show_finished) {
  return {
    project_id: "p", keep_warm: false, show_finished, paused: false, min_agents: 1, max_agents: 6, harnesses: [], messages: [], plan: [],
    events: { recent: [], waiting: [], notifications: [], paused: false, wake: null, questions: [] },
    agents: [
      agent(1, "working", { role: "master", label: "Master" }),
      agent(2, "working", { label: "Ada (@charts)", task: 20 }),
      agent(3, "starting"),
      agent(4, "idle", { finished: true, heartbeat_ms: 50 * MINUTE, register: "a".repeat(32) }),
      agent(5, "stopped", { finished: true, stopped_ms: 40 * MINUTE, register: "b".repeat(32) }),
      agent(6, "failed", { finished: true, stopped_ms: 60 * MINUTE - 30_000, problem: "The agent process closed." }),
    ],
    tasks: [
      task(20, "running", { agent: 2 }),
      task(21, "done", { agent: 5, prompt: "Write the docs\nwith care", result: "# Docs\nWrote the guide." }),
      task(22, "done", { agent: 5, prompt: "Fix the links", result: "Fixed 3 links." }),
      task(23, "done", { agent: 4, prompt: "Read the plan" }),
    ],
  };
}

test("Hide keeps only working and waiting agents in the Team panel and moves finished ones to the fold-out", () => {
  const status = teamStatus(false);
  const shown = research.teamPanel(status, [], (id) => id, "all", 60 * MINUTE).agents.map((row) => row.name);
  assert.deepEqual(shown, ["Master", "Ada (@charts)", "Sub-agent 3"]);
  const rows = research.finishedRows(status, (id) => `${id}!`, 60 * MINUTE);
  assert.deepEqual(rows.map((row) => [row.agent, row.word, row.wake]), [[6, "Failed", null], [5, "Stopped", "b".repeat(32)], [4, "Ready", "a".repeat(32)]]);
  assert.deepEqual(rows[1], {
    key: "finished-5", agent: 5, name: "Sub-agent 5", harness: "codex!", word: "Stopped", task: "Last task: Fix the links", result: "Fixed 3 links.",
    ended: "Stopped 20 min ago", tasks: 2, wake: "b".repeat(32),
  });
  assert.equal(rows[0].task, "The agent process closed.");
  assert.equal(rows[0].ended, "Failed just now");
  assert.equal(rows[2].ended, "Last active 10 min ago");
  assert.equal(rows[2].result, null, "a done task without a result offers no result");
  assert.equal(research.finishedLabel(rows.length), "Finished (3)");
  assert.equal(research.finishedLabel(0), "Finished (0)");
});

test("Show keeps finished agents in the Team panel and the fold-out keeps their Wake", () => {
  const status = teamStatus(true);
  assert.equal(research.teamPanel(status, [], (id) => id, "all", 0).agents.length, 6);
  assert.deepEqual(research.teamPanel(status, [], (id) => id, "running", 0).agents.map((row) => row.name), ["Master", "Ada (@charts)", "Sub-agent 3", "Sub-agent 4"]);
  assert.deepEqual(research.finishedRows(status, (id) => id, 0).map((row) => row.agent), [6, 5, 4]);
  const older = { ...status, show_finished: undefined, agents: status.agents.map(({ finished, ...rest }) => rest) };
  assert.equal(research.teamPanel(older, [], (id) => id, "all", 0).agents.length, 6, "an engine without the flag hides nothing");
  assert.deepEqual(research.finishedRows(older, (id) => id, 0), []);
});

test("the fold-out shows the count, each agent's result and a Wake button that needs a register entry", () => {
  const data = { id: "p", now: 60 * MINUTE, notify() {} };
  const html = renderToStaticMarkup(createElement(team.FinishedAgentsPanel, {
    data, team: { result: { kind: "ok", value: teamStatus(false) }, at: 60 * MINUTE }, harnesses: null, reload() {},
    children: createElement("p", null, "cleanup here"),
  }));
  assert.match(html, /^<details/);
  assert.match(html, /<strong>Finished \(3\)<\/strong>/);
  assert.equal(html.match(/>Wake<\/button>/g)?.length, 3);
  assert.equal(html.match(/<button[^>]*disabled=""[^>]*title="CrazyAgent has no record to wake this agent from\."/g)?.length, 1);
  assert.equal(html.match(/>Show result<\/button>/g)?.length, 1);
  assert.match(html, /2 tasks run\./);
  assert.match(html, /cleanup here/);
  const idle = { ...teamStatus(false), agents: teamStatus(false).agents.map((item) => ({ ...item, finished: false })) };
  const none = renderToStaticMarkup(createElement(team.FinishedAgentsPanel, {
    data, team: { result: { kind: "ok", value: idle }, at: 0 }, harnesses: null, reload() {},
  }));
  assert.equal(none, "");
});

test("the team controls offer Finished agents Hide and Show with Hide the default", () => {
  const render = (show_finished) => renderToStaticMarkup(createElement(controls.TeamControls, { projectId: "p", status: teamStatus(show_finished), reload() {} }));
  const checked = (html) => html.match(/role="radiogroup" aria-label="Finished agents">(.*?)<\/div>/)[1].match(/aria-checked="true"[^>]*>([A-Za-z]+)</)[1];
  assert.equal(checked(render(undefined)), "Hide");
  assert.equal(checked(render(false)), "Hide");
  assert.equal(checked(render(true)), "Show");
});

test("the Wake button restarts through the agent register's existing restart command", () => {
  const source = readFileSync(new URL("../src/screens/project/team.tsx", import.meta.url), "utf8");
  assert.match(source, /await restartAgent\(row\.wake\)/);
  const client = readFileSync(new URL("../src/engine-client.ts", import.meta.url), "utf8");
  assert.match(client, /invoke<IntentReceipt>\("desktop_team_finished_agents", \{ id, show, generation \}\)/);
});
