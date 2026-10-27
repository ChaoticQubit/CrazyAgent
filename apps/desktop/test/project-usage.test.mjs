import "./load-source.mjs";
import assert from "node:assert/strict";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";

const card = await import("../src/screens/project/usage.tsx");

const tokens = (input) => ({ input, output: 0, cache_read: 0, cache_write: 0 });
const row = (key, input) => ({ key, tokens: tokens(input), runs: 1, last_ms: 0 });
const lines = (agent, added, removed) => ({ agent, change_sets: 1, added, removed, commits: 0, committed_added: 0, committed_removed: 0 });
const report = (agents, agentLines = []) => ({
  since_ms: null, total: tokens(0), runs: 0, projects: [], pools: [], accounts: [], agents, days: [], lines: [], agent_lines: agentLines, limits: [],
});
const team = [
  { id: 1, role: "master", label: "Team master" },
  { id: 3, role: "sub_agent", label: "Ada (@login-page)" },
  { id: 4, role: "sub_agent", label: "Grace (@signup)" },
];
const today = report([row("team-3", 1200)], [lines("team-3", 10, 2)]);
const week = report(
  [row("team-3", 4000), row("master-harbor", 900), row("research-scout", 50), row("onboarding", 20)],
  [lines("team-3", 40, 9), lines("team-4", 5, 0), lines("master-harbor", 3, 1)],
);
const text = (markup) => markup.replace(/<[^>]*>/gu, " ").replace(/&#x27;/gu, "'").replace(/&amp;/gu, "&").replace(/\s+/gu, " ");

test("each agent with tokens or lines in 7 days gets one row named as the team names it", () => {
  const rows = card.agentUsage(today, week, team);
  assert.deepEqual(rows.map((r) => r.name), ["Ada (@login-page)", "Team master", "Research scout", "Onboarding", "Grace (@signup)"]);
  assert.deepEqual(rows[0], {
    key: "team-3", name: "Ada (@login-page)",
    today: { tokens: 1200, added: 10, removed: 2 },
    week: { tokens: 4000, added: 40, removed: 9 },
  });
  assert.deepEqual(rows[1].today, { tokens: 0, added: 0, removed: 0 });
  assert.deepEqual(rows[1].week, { tokens: 900, added: 3, removed: 1 });
  assert.deepEqual(rows[4].week, { tokens: 0, added: 5, removed: 0 });
  assert.equal(card.agentUsage(today, week, [])[1].name, "Master");
  assert.equal(card.agentUsage(report([row("team-8", 5)]), report([row("team-8", 5)]), team)[0].name, "Agent 8");
});

test("the Usage card shows tokens and lines today and over 7 days, and its loading, empty and failed states", () => {
  const words = text(renderToStaticMarkup(createElement(card.AgentUsageView, { today: { kind: "ok", value: today }, week: { kind: "ok", value: week }, agents: team })));
  assert.match(words, /Usage/);
  assert.match(words, /Agent Tokens today Lines today Tokens, 7 days Lines, 7 days/);
  assert.match(words, /Ada \(@login-page\) 1\.2K \+10 -2 4K \+40 -9/);
  assert.match(words, /Team master 0 \+0 -0 900 \+3 -1/);
  assert.match(words, /Grace \(@signup\) 0 \+0 -0 0 \+5 -0/);
  const empty = text(renderToStaticMarkup(createElement(card.AgentUsageView, { today: { kind: "ok", value: report([]) }, week: { kind: "ok", value: report([]) }, agents: team })));
  assert.match(empty, /No agent used tokens or changed lines in the last 7 days\./);
  assert.match(renderToStaticMarkup(createElement(card.AgentUsageView, { today: { kind: "loading" }, week: { kind: "ok", value: week }, agents: team })), /skeleton/);
  const failed = text(renderToStaticMarkup(createElement(card.AgentUsageView, { today: { kind: "ok", value: today }, week: { kind: "error", code: "usage_outcome_unknown", message: "" }, agents: team })));
  assert.match(failed, /Couldn't load usage\. CrazyAgent couldn't confirm what happened/);
  assert.equal(renderToStaticMarkup(createElement(card.AgentUsageView, { today: { kind: "not_ready" }, week: { kind: "not_ready" }, agents: team })), "");
  assert.equal(renderToStaticMarkup(createElement(card.AgentUsageView, { today: { kind: "ok", value: today }, week: { kind: "error", code: "usage_unavailable", message: "" }, agents: team })), "");
});
