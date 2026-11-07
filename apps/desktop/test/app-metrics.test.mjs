import "./load-source.mjs";
import assert from "node:assert/strict";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";

const view = await import("../src/screens/metrics/view.tsx");
const appPage = await import("../src/screens/metrics/index.tsx");
const router = await import("../src/ui/router.ts");
const shell = await import("../src/ui/shell.tsx");
const app = await import("../src/app.tsx");

const MINUTE = 60_000;
const HOUR = 60 * MINUTE;
const SINCE = Date.UTC(2026, 10, 7);
const NOW = SINCE + 9 * HOUR;
const forbidden = () => { throw new Error("static rendering must not call the engine"); };
const tokens = (input, cache_write, output, cache_read) => ({ input, output, cache_read, cache_write });
const ALPHA = tokens(700, 500, 200, 3000);
const BETA = tokens(500, 300, 100, 1000);
const PROJECTS = [{ id: "alpha", name: "Alpha" }, { id: "beta", name: "Beta" }];
const usage = {
  since_ms: SINCE, total: tokens(1200, 800, 300, 4000), runs: 3, projects: [], pools: [], accounts: [], agents: [], days: [],
  lines: [{ project: "alpha", change_sets: 3, added: 90, removed: 57, commits: 2, committed_added: 80, committed_removed: 55 }],
  agent_lines: [], limits: [], partial_homes: 0,
};
const metrics = {
  running: 2, tasks_waiting: 1, tasks_done: 6, tasks_failed: 0, bucket_ms: 2 * HOUR,
  series: [{ start_ms: SINCE, tokens: tokens(100, 50, 20, 999) }],
  runs: [
    { project: "alpha", agent: "team-8", session: "rollout-abc", task: 142, title: "Calendar API endpoints", status: "running", last_ms: NOW - 5 * MINUTE, tokens: ALPHA },
    { project: "beta", agent: "master-beta", session: "m-2", task: null, title: null, status: null, last_ms: NOW - 11 * MINUTE, tokens: BETA },
  ],
  projects: [
    { key: "alpha", tokens: ALPHA, agents: 2, runs: 3, projects: ["alpha"] },
    { key: "beta", tokens: BETA, agents: 1, runs: 1, projects: ["beta"] },
  ],
  agents: [
    { project: "alpha", agent: "team-8", harnesses: ["codex"], accounts: ["acct-a", "acct-b"], tokens: ALPHA },
    { project: "beta", agent: "master-beta", harnesses: ["claude-code"], accounts: ["acct-a"], tokens: BETA },
  ],
  accounts: [{ key: "acct-a", tokens: tokens(1200, 800, 300, 4000), agents: 2, runs: 2, projects: ["alpha", "beta"] }],
  harnesses: [
    { key: "codex", tokens: ALPHA, agents: 1, runs: 1, projects: ["alpha"] },
    { key: "claude-code", tokens: BETA, agents: 1, runs: 1, projects: ["beta"] },
  ],
};
const names = { agents: new Map([["team-8", "Juniper"]]), accounts: new Map([["acct-a", "Pool A, Claude 1"]]) };
const render = (patch = {}) => renderToStaticMarkup(createElement(view.MetricsPage, {
  projectId: null, usage: { kind: "ok", value: usage }, metrics: { kind: "ok", value: metrics }, adoption: null,
  range: "today", by: "project", names, sinceMs: SINCE, nowMs: NOW, updatedMs: NOW - 10_000, offset: 0,
  filter: { projects: PROJECTS, onProject: forbidden },
  onRange: forbidden, onBy: forbidden, onRetry: forbidden, ...patch,
}));
const text = (markup) => markup.replace(/<[^>]*>/gu, " ").replace(/&#x27;/gu, "'").replace(/&amp;/gu, "&").replace(/\s+/gu, " ");
const tableText = (markup) => text(markup.slice(markup.indexOf("<table")));

test("the app Metrics page covers every project on the engine and filters by project", () => {
  const markup = renderToStaticMarkup(createElement(appPage.AppMetrics, { projects: PROJECTS }));
  assert.match(markup, /<h1>Metrics<\/h1><p>Every project on this engine in one place\.<\/p>/);
  assert.match(markup, /<label for="mx-project" class="visually-hidden">Project<\/label><select id="mx-project"/);
  assert.deepEqual([...markup.matchAll(/<option value="([^"]*)"[^>]*>([^<]*)</gu)].map((match) => [match[1], match[2]]), [["", "All projects"], ["alpha", "Alpha"], ["beta", "Beta"]]);
  assert.match(text(markup), /2 projects · Updating · provider-reported usage per turn/);
});

test("all projects shows the same tiles, and By project lists each project with its agents and runs first", () => {
  const markup = render();
  assert.match(text(markup), /Tokens 2\.3K 1\.2K input, 800 cache write, 300 output/);
  assert.match(text(markup), /2 projects · Updated 10 s ago · provider-reported usage per turn/);
  assert.match(text(markup), /By project By agent By run By account By harness/);
  const table = tableText(markup);
  assert.match(table, /^ Project Tokens Input Cache write Output Cache read Agents Runs Share of tokens /);
  assert.match(table, /Alpha 1\.4K 700 500 200 3K 2 3 61%/);
  assert.match(table, /Beta 900 500 300 100 1K 1 1 39%/);
});

test("across all projects, By agent and By run add a Project column, and an agent names its harness and accounts", () => {
  const agents = tableText(render({ by: "agent" }));
  assert.match(agents, /^ Agent Project Tokens Input Cache write Output Cache read Harness Account Share of tokens /);
  assert.match(agents, /Juniper Alpha 1\.4K 700 500 200 3K Codex Pool A, Claude 1, acct-b 61%/);
  assert.match(agents, /Master, Beta Beta 900 500 300 100 1K Claude Code Pool A, Claude 1 39%/);
  assert.doesNotMatch(agents, /graphify|ctx_\*/);
  const runs = tableText(render({ by: "run" }));
  assert.match(runs, /^ Run Project Tokens Input Cache write Output Cache read Agent Ended Result Share of tokens /);
  assert.match(runs, /#142 Calendar API endpoints Alpha 1\.4K 700 500 200 3K Juniper running Working 61%/);
  assert.match(runs, /No task Beta 900 500 300 100 1K Master, Beta 11 min ago 39%/);
});

test("By account and By harness name the projects that used each one", () => {
  assert.match(tableText(render({ by: "account" })), /^ Account Tokens Input Cache write Output Cache read Agents Projects Share of tokens Pool A, Claude 1 2\.3K 1\.2K 800 300 4K 2 Alpha, Beta 100%/);
  const harnesses = tableText(render({ by: "harness" }));
  assert.match(harnesses, /^ Harness Tokens Input Cache write Output Cache read Agents Projects Share of tokens /);
  assert.match(harnesses, /Codex 1\.4K 700 500 200 3K 1 Alpha 61%/);
  assert.match(harnesses, /Claude Code 900 500 300 100 1K 1 Beta 39%/);
});

test("a picked project shows its name, stays picked and drops the Project column", () => {
  const markup = render({ projectId: "alpha", by: "agent" });
  assert.match(text(markup), /Alpha · Updated 10 s ago · provider-reported usage per turn/);
  assert.match(markup, /<option value="alpha" selected="">Alpha<\/option>/);
  assert.match(tableText(markup), /^ Agent Tokens Input Cache write Output Cache read Harness Account Share of tokens /);
});

test("the sidebar's App section opens Metrics at #/metrics, below the main items", () => {
  assert.deepEqual(router.parseRoute("#/metrics"), { name: "metrics" });
  assert.equal(router.formatRoute({ name: "metrics" }), "#/metrics");
  assert.deepEqual(router.parseRoute("#/metrics/extra"), { name: "home" });
  const sidebar = (route) => renderToStaticMarkup(createElement(shell.Sidebar, { route, projects: PROJECTS, profileName: "Sam", engine: "running", onRetry: forbidden }));
  const markup = sidebar({ name: "metrics" });
  assert.match(markup, /<\/nav><nav class="nav app-nav" aria-label="App"><a class="nav-item" href="#\/metrics" aria-current="page"><svg[\s\S]*?<\/svg><span class="name">Metrics<\/span><\/a><\/nav><div class="sidebar-foot">/);
  assert.doesNotMatch(sidebar({ name: "home" }), /href="#\/metrics" aria-current/);
});

test("the Metrics route shows the app Metrics page with the engine's projects", () => {
  const screen = app.Screen({ route: { name: "metrics" }, session: { projects: { kind: "ok", value: PROJECTS } } });
  assert.equal(screen.type, appPage.AppMetrics);
  assert.deepEqual(screen.props.projects, PROJECTS);
});
