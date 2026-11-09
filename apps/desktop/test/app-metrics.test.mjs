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
const { SessionContext } = await import("../src/state.ts");

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
  running: 2, tasks_waiting: 1, tasks_done: 6, tasks_failed: 0, merges: 19, merges_into_main: 5, merges_undone: 1, bucket_ms: 2 * HOUR,
  series: [{ start_ms: SINCE, tokens: tokens(100, 50, 20, 999) }],
  runs: [
    { project: "alpha", agent: "team-8", session: "rollout-abc", task: 142, title: "Calendar API endpoints", status: "running", last_ms: NOW - 5 * MINUTE, tokens: ALPHA },
    { project: "beta", agent: "master-beta", session: "m-2", task: null, title: null, status: null, last_ms: NOW - 11 * MINUTE, tokens: BETA },
  ],
  projects: [
    { key: "alpha", tokens: ALPHA, agents: 2, runs: 3, projects: ["alpha"], merges: 4 },
    { key: "beta", tokens: BETA, agents: 1, runs: 1, projects: ["beta"], merges: 0 },
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
const LIMITS = { pools: new Map(), failure: "", busy: null, next: null, revision: 0, onRefresh: forbidden, onSetReset: forbidden };
const FILTER = { projects: PROJECTS, onProject: forbidden, limits: LIMITS };
const SESSION = { projects: { kind: "loading" }, noteResult: (result) => result, showToast() {} };
const render = (patch = {}) => renderToStaticMarkup(createElement(view.MetricsPage, {
  projectId: null, usage: { kind: "ok", value: usage }, metrics: { kind: "ok", value: metrics }, adoption: null,
  range: "today", by: "project", names, sinceMs: SINCE, nowMs: NOW, updatedMs: NOW - 10_000, offset: 0,
  filter: FILTER,
  onRange: forbidden, onBy: forbidden, onRetry: forbidden, ...patch,
}));
const text = (markup) => markup.replace(/<[^>]*>/gu, " ").replace(/&#x27;/gu, "'").replace(/&amp;/gu, "&").replace(/\s+/gu, " ");
const tableText = (markup) => text(markup.slice(markup.lastIndexOf("<table")));

test("the app Metrics page covers every project on the engine and filters by project", () => {
  const markup = renderToStaticMarkup(createElement(SessionContext.Provider, { value: SESSION }, createElement(appPage.AppMetrics, { projects: PROJECTS })));
  assert.match(markup, /<h1>Metrics<\/h1><p>Every project on this engine in one place\.<\/p>/);
  assert.match(markup, /<label for="mx-project" class="visually-hidden">Project<\/label><select id="mx-project"/);
  assert.deepEqual([...markup.matchAll(/<option value="([^"]*)"[^>]*>([^<]*)</gu)].map((match) => [match[1], match[2]]), [["", "All projects"], ["alpha", "Alpha"], ["beta", "Beta"]]);
  assert.match(text(markup), /2 projects · Updating · provider-reported usage per turn/);
});

test("all projects shows the same tiles, and By project lists each project with its agents and runs first", () => {
  const markup = render();
  assert.match(text(markup), /Tokens 2\.3K Input 1\.2K Cache write 800 Output 300/);
  assert.match(text(markup), /2 projects · Updated 10 s ago · provider-reported usage per turn/);
  assert.match(text(markup), /Tasks 6 done 1 waiting, 0 failed Merges 19 5 into main, 1 undone Lines /, "the project tab's tiles under the lead panel");
  assert.match(text(markup), /By project By agent By run By account By harness/);
  const table = tableText(markup);
  assert.match(table, /^ Project Tokens Input Cache write Output Cache read Agents Runs Merges Share of tokens /, "By project counts each project's merges, as the mockup does");
  assert.match(table, /Alpha 1\.4K 700 500 200 3K 2 3 4 61%/);
  assert.match(table, /Beta 900 500 300 100 1K 1 1 0 39%/);
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

test("the sidebar's App section opens Metrics at #/metrics, last of its links", () => {
  assert.deepEqual(router.parseRoute("#/metrics"), { name: "metrics" });
  assert.equal(router.formatRoute({ name: "metrics" }), "#/metrics");
  assert.deepEqual(router.parseRoute("#/metrics/extra"), { name: "home" });
  const sidebar = (route) => renderToStaticMarkup(createElement(shell.Sidebar, { route, projects: PROJECTS, profileName: "Sam", engine: "running", onRetry: forbidden }));
  const markup = sidebar({ name: "metrics" });
  assert.match(markup, /<\/nav><nav class="nav app-nav" aria-label="App">[\s\S]*<a class="nav-item" href="#\/metrics" aria-current="page"><svg[\s\S]*?<\/svg><span class="name">Metrics<\/span><\/a><\/nav><div class="sidebar-foot">/);
  assert.doesNotMatch(sidebar({ name: "home" }), /href="#\/metrics" aria-current/);
});

test("the Metrics route shows the app Metrics page with the engine's projects", () => {
  const screen = app.Screen({ route: { name: "metrics" }, session: { projects: { kind: "ok", value: PROJECTS } } });
  assert.equal(screen.type, appPage.AppMetrics);
  assert.deepEqual(screen.props.projects, PROJECTS);
});

test("a Projects, Harness or Account cell lists the names the read kept, then how many more there were", () => {
  const eight = Array.from({ length: 8 }, (_, at) => `p-${at}`);
  const capped = {
    ...metrics,
    accounts: [{ ...metrics.accounts[0], projects: eight, more_projects: 3 }],
    harnesses: [{ ...metrics.harnesses[0], more_projects: 0 }],
    agents: [{ ...metrics.agents[0], more_harnesses: 0, more_accounts: 2 }],
  };
  const shown = (by) => tableText(render({ by, metrics: { kind: "ok", value: capped } }));
  assert.match(shown("account"), /p-0, p-1, p-2, p-3, p-4, p-5, p-6, p-7, \+3 more/);
  assert.match(shown("agent"), /Codex Pool A, Claude 1, acct-b, \+2 more/);
  assert.doesNotMatch(shown("harness"), /more/);
});

test("the app page names agents from the metrics read and reads no team status per project", () => {
  const filter = { projects: PROJECTS, onProject: forbidden };
  assert.deepEqual(view.namedProjects(null, filter), []);
  assert.deepEqual(view.namedProjects("alpha", filter), []);
  assert.deepEqual(view.namedProjects("alpha", undefined), ["alpha"]);
  assert.deepEqual(view.namedProjects(null, undefined), []);
  const label = (agent) => agent === "team-8" ? "Ada (@calendar)" : "Master";
  const labelled = {
    ...metrics,
    runs: metrics.runs.map((run) => ({ ...run, label: label(run.agent) })),
    agents: metrics.agents.map((row) => ({ ...row, label: label(row.agent) })),
  };
  const shown = (by) => tableText(render({ by, names: { agents: new Map(), accounts: names.accounts }, metrics: { kind: "ok", value: labelled } }));
  assert.match(shown("agent"), /Ada \(@calendar\) Alpha 1\.4K/);
  assert.match(shown("agent"), /Master Beta 900/);
  assert.match(shown("run"), /#142 Calendar API endpoints Alpha 1\.4K 700 500 200 3K Ada \(@calendar\) /);
});

test("across all projects By project and By agent total the Tokens tile, an Other row holding what the listed rows leave", () => {
  const listed = { ...metrics, projects: [metrics.projects[0]], agents: [metrics.agents[0]] };
  const shown = (by) => tableText(render({ by, metrics: { kind: "ok", value: listed } }));
  for (const by of ["project", "agent"]) {
    assert.match(shown(by), /Other, not listed 900 500 300 100 1K 39%/, by);
    assert.match(shown(by), /Total 2\.3K 1\.2K 800 300 4K/, by);
  }
  assert.doesNotMatch(tableText(render()), /Other, not listed/);
});

const DAY = 24 * HOUR;
const limitsFor = (patch = {}) => ({
  account: "acct-a", plan: "Max", source: "poll", observed_ms: NOW - 2 * MINUTE, failure: null,
  five_hour: { used_percent: 84, resets_at_ms: NOW + 2 * HOUR }, weekly: { used_percent: 12, resets_at_ms: NOW + 3 * DAY },
  five_hour_reset_override_ms: null, weekly_reset_override_ms: null, five_hour_left_out_label: null, weekly_left_out_label: null, ...patch,
});
const withUsage = (patch) => ({ usage: { kind: "ok", value: { ...usage, ...patch } } });

test("the app page says the numbers still use the old counting and what it counts", () => {
  const words = text(render());
  assert.match(words, /These numbers still use the old counting\. The recount comes in the next update, and you will see old and new side by side once\./);
  assert.match(words, /Tokens your agents used and each account's limits\. Counts come from each run's own record; limits are checked every 10 minutes\./);
});

test("the project tab keeps its own page: no old-counting notice and no Account limits", () => {
  const words = text(render({ filter: undefined, projectId: "alpha", by: "agent" }));
  assert.doesNotMatch(words, /old counting/);
  assert.doesNotMatch(words, /Account limits/);
  assert.doesNotMatch(words, /Check limits now/);
});

test("Account limits has Check limits now, a meter and the clock time of each reset", () => {
  const markup = render(withUsage({ limits: [limitsFor()] }));
  assert.match(markup, /<h2>Account limits<\/h2><button[^>]*>[\s\S]*?Check limits now/);
  assert.match(markup, /<meter class="usage-meter"/);
  const words = text(markup);
  assert.match(words, /Pool A, Claude 1/);
  assert.match(words, /5-hour 84% used Near limit/);
  assert.match(words, /Resets at (?:[A-Z][a-z]{2} )?\d\d:\d\d, in 2 h 0 min/);
  assert.match(words, /Weekly 12% used Resets at [A-Z][a-z]{2} \d\d:\d\d, in 3 d 0 h/);
  assert.match(words, /Set reset time/);
});

test("a window reads as near its limit from 80 percent", async () => {
  const limits = await import("../src/screens/metrics/limits.tsx");
  assert.equal(limits.nearLimit(79.9), false);
  assert.equal(limits.nearLimit(80), true);
  assert.equal(limits.resetAtText(null, NOW), "Next reset not known yet");
});

test("a failed action, an internal error and a partial reading all show on the app page", () => {
  const health = [{ step: "poll", panics: 2, last_ms: NOW - 5 * MINUTE, recovered_ms: null }];
  const words = text(render({ ...withUsage({ health, partial_homes: 1 }), filter: { ...FILTER, limits: { ...LIMITS, failure: "The check failed." } } }));
  assert.match(words, /The check failed\./);
  assert.match(words, /Checking account limits hit an internal error 2 times/);
  assert.match(words, /Some usage may be missing: 1 agent home/);
});

test("no limits read yet says so, and the next accounts panel follows Account limits", () => {
  const next = createElement("p", null, "Next accounts here");
  const words = text(render({ filter: { ...FILTER, limits: { ...LIMITS, next } } }));
  assert.match(words, /No limits read yet\. Signed-in Claude and ChatGPT accounts show here after their first check\. Next accounts here/);
});

test("Check limits now stays reachable while loading and after a failed read, and shows when it is asking", () => {
  assert.match(text(render({ usage: { kind: "loading" } })), /Check limits now/);
  assert.match(text(render({ usage: { kind: "error", code: "x", message: "y" } })), /Check limits now/);
  assert.match(text(render(withUsage({ limits: [limitsFor()] }))), /Check limits now/);
  assert.match(text(render({ filter: { ...FILTER, limits: { ...LIMITS, busy: "refresh" } } })), /Asking\.\.\./);
});

test("the lead panel shows Tokens with its mix beside the chart, and the chart has a table with the same numbers", () => {
  const markup = render();
  const lead = markup.slice(markup.indexOf('class="panel mx-lead"'));
  const words = text(lead);
  assert.match(words, /Tokens 2\.3K/);
  assert.match(words, /Input 1\.2K Cache write 800 Output 300 Re-read from cache 4K/);
  assert.match(lead, /aria-live="polite"/);
  assert.match(lead, /role="img"/);
  assert.match(lead, /<details class="mx-more"><summary>Show chart as a table<\/summary>/);
  assert.match(text(lead.slice(lead.indexOf("<details"))), /Tokens Input Cache write Output Cache read/);
});

test("only unreported usage reads as not reported in the lead, never as zero", () => {
  const none = tokens(0, 0, 0, 0);
  assert.match(text(render(withUsage({ total: none, not_reported_by: ["Codex"] }))), /Tokens Not reported Usage not reported by Codex/);
});

test("usage some harness did not report stays under the Tokens", () => {
  assert.match(text(render(withUsage({ not_reported_by: ["Codex"] }))), /Not in these totals: usage not reported by Codex\./);
});

test("the tiles under the lead are Re-read from cache, Runs, Tasks, Merges and Lines", () => {
  const keys = [...render().matchAll(/mx-tile-key">([^<]*)</gu)].map((match) => match[1]);
  assert.deepEqual(keys, ["Re-read from cache", "Runs", "Tasks", "Merges", "Lines"]);
});

test("the app page has All and the project tab does not", () => {
  assert.match(text(render()), /Today 7 days 30 days All/);
  assert.doesNotMatch(text(render({ filter: undefined, projectId: "alpha", by: "agent" })), /Today 7 days 30 days All/);
});

test("Account limits and its reset controls stay when only the metrics read failed (r1 L1)", () => {
  const next = createElement("p", null, "Next accounts here");
  const failed = { kind: "error", code: "usage_outcome_unknown", message: "" };
  const markup = render({ ...withUsage({ limits: [limitsFor()] }), metrics: failed, filter: { ...FILTER, limits: { ...LIMITS, next } } });
  const words = text(markup);
  assert.match(words, /Account limits/);
  assert.match(words, /Set reset time/);
  assert.match(words, /Next accounts here/);
  assert.match(words, /Try again/);
  assert.equal(markup.split("Check limits now").length - 1, 1, "one Check limits now");
  assert.doesNotMatch(text(render({ usage: failed })), /Account limits/, "no usage read, no limits to show");
});

test("Account limits stays while the metrics read loads or is not ready (r1 L1)", () => {
  const notReady = { kind: "error", code: "usage_unavailable", message: "" };
  for (const metrics of [{ kind: "loading" }, { kind: "not_ready" }, notReady]) {
    const markup = render({ ...withUsage({ limits: [limitsFor()] }), metrics });
    assert.match(text(markup), /Account limits/, JSON.stringify(metrics));
    assert.match(text(markup), /Set reset time/, JSON.stringify(metrics));
    assert.equal(markup.split("Check limits now").length - 1, 1, JSON.stringify(metrics));
  }
  assert.match(text(render({ ...withUsage({ limits: [limitsFor()] }), metrics: notReady })), /isn't ready yet/);
});

test("a refused Check limits says why on the failed-read page (r1 L2)", () => {
  const failed = { kind: "error", code: "usage_outcome_unknown", message: "" };
  const filter = { ...FILTER, limits: { ...LIMITS, failure: "The check failed." } };
  assert.match(text(render({ usage: failed, filter })), /The check failed\./);
  assert.match(text(render({ metrics: failed, filter })), /The check failed\./);
});

test("a picked project keeps the app page's notice and Account limits (r1 L3)", () => {
  const words = text(render({ projectId: "alpha", ...withUsage({ limits: [limitsFor()] }) }));
  assert.match(words, /These numbers still use the old counting\./);
  assert.match(words, /Account limits/);
  assert.match(words, /Set reset time/);
});
