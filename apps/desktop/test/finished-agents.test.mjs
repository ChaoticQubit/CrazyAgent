import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import "./load-source.mjs";

const research = await import("../src/screens/project/research.ts");
const team = await import("../src/screens/project/team.tsx");
const controls = await import("../src/screens/team/controls.tsx");
const state = await import("../src/state.ts");

const MINUTE = 60_000;
const agent = (id, status, extra = {}) => ({ id, role: "sub", label: `Sub-agent ${id}`, harness: "codex", tools: true, account: null, worktree: null, status, status_label: "",
  task: null, turns: 0, problem: null, process_group: null, created_ms: 0, heartbeat_ms: 0, stopped_ms: null, finished: false, wakeable: false, wake_note: null, ...extra });
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
      agent(4, "idle", { finished: true, turns: 1, heartbeat_ms: 50 * MINUTE, wake_note: "Already running." }),
      agent(5, "stopped", { finished: true, stopped_ms: 40 * MINUTE, wakeable: true }),
      agent(6, "failed", { finished: true, stopped_ms: 60 * MINUTE - 30_000, problem: "The agent process closed.",
        wake_note: "Its worktree was cleaned up, so it cannot be woken. Hand its work to a new agent." }),
    ],
    tasks: [
      task(20, "running", { agent: 2 }),
      task(21, "done", { agent: 5, prompt: "Write the docs\nwith care", result: "# Docs\nWrote the guide." }),
      task(22, "done", { agent: 5, prompt: "Fix the links", result: "Fixed 3 links." }),
      task(23, "done", { agent: 4, prompt: "Read the plan", finished_ms: 45 * MINUTE }),
    ],
  };
}

test("Hide keeps only working and waiting agents in the Team panel and moves finished ones to the fold-out", () => {
  const status = teamStatus(false);
  const shown = research.teamPanel(status, [], (id) => id, "all", 60 * MINUTE).agents.map((row) => row.name);
  assert.deepEqual(shown, ["Master", "Ada (@charts)", "Sub-agent 3"]);
  const rows = research.finishedRows(status, (id) => `${id}!`, 60 * MINUTE);
  assert.deepEqual(rows.map((row) => [row.agent, row.word, row.wakeable, row.wakeNote]), [
    [6, "Failed", false, "Its worktree was cleaned up, so it cannot be woken. Hand its work to a new agent."],
    [5, "Stopped", true, null],
    [4, "Ready", false, "Already running."],
  ]);
  assert.deepEqual(rows[1], {
    key: "finished-5", agent: 5, name: "Sub-agent 5", harness: "codex!", word: "Stopped", task: "Last task: Fix the links", result: "Fixed 3 links.",
    ended: "Stopped 20 min ago", tasks: 2, wakeable: true, wakeNote: null,
  });
  assert.equal(rows[0].task, "The agent process closed.");
  assert.equal(rows[0].ended, "Failed just now");
  assert.equal(rows[2].ended, "Last active 15 min ago", "an idle agent is last active when its last task finished, not at its heartbeat");
  const fresh = { ...status, agents: [agent(7, "idle", { finished: true, created_ms: 30 * MINUTE, heartbeat_ms: 60 * MINUTE })] };
  assert.equal(research.finishedRows(fresh, (id) => id, 60 * MINUTE)[0].ended, "Last active 30 min ago", "an idle agent with no task is last active when it started");
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
  const unsaid = { ...status, agents: status.agents.map(({ wakeable, wake_note, ...rest }) => rest) };
  assert.deepEqual(research.finishedRows(unsaid, (id) => id, 0).map((row) => [row.wakeable, row.wakeNote]),
    Array(3).fill([false, "This version of the engine can't wake agents."]), "an engine that does not say an agent wakes offers no Wake");
});

test("the fold-out shows the count, each agent's result and a Wake button with the engine's reason when it can't wake", () => {
  const data = { id: "p", now: 60 * MINUTE, notify() {} };
  const html = renderToStaticMarkup(createElement(team.FinishedAgentsPanel, {
    data, team: { result: { kind: "ok", value: teamStatus(false) }, at: 60 * MINUTE }, harnesses: null, reload() {},
    children: createElement("p", null, "cleanup here"),
  }));
  assert.match(html, /^<details/);
  assert.match(html, /<strong>Finished \(3\)<\/strong>/);
  assert.equal(html.match(/>Wake<\/button>/g)?.length, 3);
  assert.equal(html.match(/<button[^>]*disabled=""[^>]*title="[^"]+"[^>]*>Wake</g)?.length, 2);
  assert.match(html, /title="Already running\."/);
  assert.match(html, /Its worktree was cleaned up, so it cannot be woken\. Hand its work to a new agent\./);
  assert.equal(html.match(/<button type="button" class="btn small">Wake<\/button>/g)?.length, 1, "only the stopped agent with its worktree wakes");
  assert.equal(html.match(/>Show result<\/button>/g)?.length, 1);
  assert.match(html, /2 tasks run\./);
  assert.match(html, /cleanup here/);
  const idle = { ...teamStatus(false), agents: teamStatus(false).agents.map((item) => ({ ...item, finished: false })) };
  const none = renderToStaticMarkup(createElement(team.FinishedAgentsPanel, {
    data, team: { result: { kind: "ok", value: idle }, at: 0 }, harnesses: null, reload() {},
  }));
  assert.equal(none, "");
});

test("a saved team choice stays shown until the engine reports it, and a refused one shows the status again", () => {
  const held = { choice: "show", from: "hide" };
  const kept = controls.afterSave(held, "");
  assert.deepEqual(kept, held, "an applied save keeps the choice while the reload is on its way");
  assert.equal(controls.shownChoice(kept, "hide"), "show", "the control does not flip back before the status moves");
  assert.equal(controls.shownChoice(kept, "show"), "show");
  assert.equal(controls.shownChoice(kept, "other"), "other", "once the status moves, it shows the status");
  assert.equal(controls.afterSave(held, "Not ready yet: choosing where finished agents show isn't in this build."), null, "a refused save shows the status again");
  assert.equal(controls.shownChoice(null, "hide"), "hide");
  const limits = { choice: { turn: 600, session: null, idle: null }, from: { turn: null, session: null, idle: null } };
  const same = (a, b) => a.turn === b.turn && a.session === b.session && a.idle === b.idle;
  assert.deepEqual(controls.shownChoice(controls.afterSave(limits, ""), { turn: null, session: null, idle: null }, same), limits.choice);
});

test("the team controls offer Finished agents Hide and Show with Hide the default", () => {
  const render = (show_finished) => renderToStaticMarkup(createElement(controls.TeamControls, { projectId: "p", status: teamStatus(show_finished), reload() {} }));
  const checked = (html) => html.match(/role="radiogroup" aria-label="Finished agents">(.*?)<\/div>/)[1].match(/aria-checked="true"[^>]*>([A-Za-z]+)</)[1];
  assert.equal(checked(render(undefined)), "Hide");
  assert.equal(checked(render(false)), "Hide");
  assert.equal(checked(render(true)), "Show");
});

test("the Wake button sends the team wake by agent id, never the agent register's restart", () => {
  const source = readFileSync(new URL("../src/screens/project/team.tsx", import.meta.url), "utf8");
  assert.match(source, /await wakeTeamAgent\(data\.id, row\.agent, nextGeneration\(\)\)/);
  assert.doesNotMatch(source, /restartAgent/);
  const api = readFileSync(new URL("../src/api-projects.ts", import.meta.url), "utf8");
  assert.match(api, /call<IntentReceipt>\("desktop_team_wake_agent", \{ id, agent, generation \}\)/);
  const client = readFileSync(new URL("../src/engine-client.ts", import.meta.url), "utf8");
  assert.match(client, /invoke<IntentReceipt>\("desktop_team_wake_agent", \{ id, agent, generation \}\)/);
  assert.match(client, /invoke<IntentReceipt>\("desktop_team_finished_agents", \{ id, show, version, generation \}\)/);
});

test("an applied save's hold ends at the first read that started after the save answered, even one showing the old value again (UI1-R2)", () => {
  for (const name of ["startRead", "stampRead", "answeredHold", "holdEnds"]) assert.equal(typeof state[name], "function", `state has ${name}`);
  const held = { choice: "show", from: "hide" };
  const before = { finished_agents: "hide" };
  const early = state.startRead();
  const kept = state.answeredHold(held, "");
  assert.equal(kept.choice, "show", "an applied save keeps the pick");
  state.stampRead({ kind: "ok", value: before }, early);
  assert.equal(state.holdEnds(kept, "hide", Object.is, before), false, "a read already on its way when the save answered keeps the pick");
  const after = { finished_agents: "hide" };
  state.stampRead({ kind: "ok", value: after }, state.startRead());
  assert.equal(state.holdEnds(kept, "hide", Object.is, after), true, "a read that started after the answer ends the hold, even at the old value another writer put back");
  state.stampRead({ kind: "ok", value: before }, state.startRead());
  assert.equal(state.holdEnds(kept, "hide", Object.is, before), false, "a read keeps the start it was first noted with");
  assert.equal(state.holdEnds(kept, "other", Object.is, before), true, "a read that moves the value still ends it");
  assert.equal(state.holdEnds(held, "hide", Object.is, after), false, "a hold whose save has not answered stays");
  assert.equal(state.holdEnds(kept, "hide", Object.is, undefined), false, "with no read given only a moved value ends it");
  assert.equal(state.answeredHold(held, "Refused."), null, "a refused save holds nothing");
  assert.equal(state.holdEnds(null, "hide", Object.is, after), false);
  for (const file of ["controls.tsx", "coordination.tsx"]) {
    const source = readFileSync(new URL(`../src/screens/team/${file}`, import.meta.url), "utf8");
    const calls = [...source.matchAll(/useHeld(?:<[^>]+>)?\(([^;]*?)\);/gu)].map((match) => match[1]);
    assert.ok(calls.length > 0, file);
    for (const call of calls) assert.match(call, /, status$/u, `${file} gives its hold the read: ${call}`);
  }
  const team = readFileSync(new URL("../src/screens/project/team.tsx", import.meta.url), "utf8");
  assert.match(team, /const started = startRead\(\);[\s\S]*stampRead\(result, started\);/u, "the team poller notes when each read started");
});

test("every other hold on a loaded read gives that read, so a later read ends it too (UI1-R2 r1 L2)", () => {
  const reads = [["settings/updates.tsx", "Object.is, settings"], ["process.tsx", "sameJson, settings"], ["settings/memory.tsx", "Object.is, saved"], ["project/settings.tsx", "Object.is, status"]];
  for (const [file, read] of reads) {
    const source = readFileSync(new URL(`../src/screens/${file}`, import.meta.url), "utf8");
    const calls = [...source.matchAll(/useHeld(?:<[^>]+>)?\(([^;]*?)\);/gu)].map((match) => match[1]);
    assert.equal(calls.length, 1, file);
    assert.ok(calls[0].endsWith(`, ${read}`), `${file} gives its hold the read: ${calls[0]}`);
  }
});
