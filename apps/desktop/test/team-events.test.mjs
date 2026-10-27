import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import "./load-source.mjs";

const wake = await import("../src/screens/team/wake.ts");
const panel = await import("../src/screens/team/wake-rules.tsx");
const alerts = await import("../src/screens/settings/team-alerts.tsx");

const SHIPPED = { question: true, finished: true, failed: true, change_set_ready: true, progress: false, hourly_cap: 30 };
const note = (id, project, seen = null) => ({ id, project_id: project, kind: "escalation", text: `note ${id}`, created_ms: id, seen_ms: seen });
const event = (id, kind, state, text) => ({ id, agent: 7, label: "claude-7", task: 3, kind, text, state, created_ms: id, delivered_ms: null });
const agent = (id, role) => ({
  id, role, label: role === "master" ? "Master" : `Sub-agent ${id}`, harness: "claude", tools: true, account: null, worktree: null, status: "working", status_label: "Working", task: null, turns: 0,
  problem: null, process_group: null, created_ms: 0, heartbeat_ms: 0, stopped_ms: null,
});

test("the hourly limit takes a whole number from 0 to 500 like the engine", () => {
  assert.deepEqual(wake.capInput(" 0 "), { kind: "ok", cap: 0 });
  assert.deepEqual(wake.capInput("500"), { kind: "ok", cap: 500 });
  for (const text of ["501", "-1", "2.5", "", "abc", "1000"]) assert.equal(wake.capInput(text).kind, "invalid", text);
});

test("a switch changes one kind and keeps the rest of the rule", () => {
  const changed = wake.withKind(SHIPPED, "progress", true);
  assert.deepEqual(changed, { ...SHIPPED, progress: true });
  assert.equal(SHIPPED.progress, false);
  assert.deepEqual(wake.WAKE_KINDS.map((item) => item.kind), ["question", "finished", "failed", "change_set_ready", "progress"]);
});

test("an event reads like the line the master gets and says where it went", () => {
  assert.equal(wake.eventLine(event(1, "question", "queued", "Proceed? | 1. Yes | 2. No")), "claude-7 asks: Proceed? | 1. Yes | 2. No");
  assert.equal(wake.eventLine(event(2, "change_set_ready", "delivered", "3 files")), "claude-7 has changes ready: 3 files");
  assert.equal(wake.eventState("muted"), "Not sent");
  assert.equal(wake.eventState("dropped"), "Replaced by newer events");
  assert.equal(wake.ruleSource(false, "app"), "Uses the app rule.");
  assert.equal(wake.ruleSource(true, "app"), "Set here. Reset to use the rule above it.");
});

test("the sidebar counts each project's unseen notifications and the first read never alerts", () => {
  const events = { notifications: [note(5, "b"), note(4, "a"), note(3, "a"), note(2, "a", 9)] };
  assert.deepEqual([...wake.unseenByProject(events)], [["b", 1], ["a", 2]]);
  const first = wake.freshAlerts(null, events);
  assert.deepEqual(first, { last: 5, fresh: [] });
  const later = wake.freshAlerts(first.last, { notifications: [note(7, "a"), note(6, "b", 10), ...events.notifications] });
  assert.equal(later.last, 7);
  assert.deepEqual(later.fresh.map((item) => item.id), [7]);
  assert.deepEqual(wake.freshAlerts(7, { notifications: [] }), { last: 7, fresh: [] });
});

test("the Agents tab shows the paused notice, waiting agents, events and the project and agent rules", () => {
  const status = {
    project_id: "chores", keep_warm: false, paused: false, min_agents: 1, max_agents: 3, harnesses: [], tasks: [], messages: [], plan: [],
    agents: [agent(1, "master"), agent(7, "sub")],
    events: {
      recent: [event(9, "question", "queued", "Proceed?")], waiting: [{ agent: 7, text: "Proceed? | 1. Yes" }],
      notifications: [note(2, "chores")], paused: true, questions: [{ agent: 7, count: 1 }],
      wake: { app: null, project: { ...SHIPPED, hourly_cap: 5 }, agents: [], effective: { ...SHIPPED, hourly_cap: 5 }, level: "project" },
    },
  };
  const markup = renderToStaticMarkup(createElement(panel.TeamEventsPanel, { projectId: "chores", status, reload: () => {} }));
  assert.match(markup, /Auto-wake of the master is paused/);
  assert.match(markup, /Sub-agent 7<\/strong><span>Proceed\? \| 1\. Yes/);
  assert.match(markup, /claude-7 asks: Proceed\?/);
  assert.match(markup, /Mark 1 as seen/);
  assert.match(markup, /Set here\. Reset to use the rule above it\./);
  assert.match(markup, /value="5"/);
  assert.match(markup, /<option value="7">Sub-agent 7<\/option>/);
  assert.doesNotMatch(markup, /<option value="1">/);
});

test("Settings groups the notifications by project, newest project first", () => {
  const groups = alerts.byProject([note(5, "b"), note(4, "a"), note(3, "b")]);
  assert.deepEqual(groups.map((group) => [group.projectId, group.items.map((item) => item.id)]), [["b", [5, 3]], ["a", [4]]]);
});

test("the wake rule, seen and attention commands send the argument names their Rust commands take", () => {
  const client = readFileSync(new URL("../src/api-projects.ts", import.meta.url), "utf8");
  for (const call of ['("desktop_team_events", {})', '("desktop_team_wake_rule", { target, rule, generation })', '("desktop_team_seen", { id, upto, generation })', '("desktop_team_attention", { unseen, alert })']) {
    assert.ok(client.includes(call), call);
  }
  const lib = readFileSync(new URL("../src-tauri/src/lib.rs", import.meta.url), "utf8");
  for (const [command, params] of [
    ["desktop_team_wake_rule", ["target: crazyagent::team::events::RuleTarget", "rule: Option<crazyagent::team::events::WakeRule>", "generation: u64"]],
    ["desktop_team_seen", ["id: String", "upto: i64", "generation: u64"]],
    ["desktop_team_attention", ["unseen: u32", "alert: bool"]],
  ]) {
    const start = lib.indexOf(`fn ${command}(`);
    assert.ok(start >= 0, command);
    const signature = lib.slice(start, lib.indexOf(") ->", start));
    for (const param of params) assert.ok(signature.includes(param), `${command} takes ${param}`);
  }
});

test("each notification shows when it came and opens the project's conversation", () => {
  const now = 1_800_000_000_000;
  const items = [{ ...note(8, "p 2"), created_ms: now - 3 * 60_000 }, { ...note(6, "p 2", now), created_ms: now - 2 * 3_600_000 }];
  const markup = renderToStaticMarkup(createElement(panel.NotificationList, { projectId: "p 2", notifications: items, reload: () => {}, now }));
  assert.match(markup, /New<\/span>.*note 8.*3 min ago/s);
  assert.match(markup, /Seen<\/span>.*note 6.*2 h ago/s);
  assert.equal(markup.match(/href="#\/projects\/p%202\/conversation"/g)?.length, 2);
  assert.match(markup, /Mark 1 as seen/);
});
