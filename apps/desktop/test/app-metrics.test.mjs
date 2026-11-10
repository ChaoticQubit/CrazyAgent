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
const { dayLabel } = await import("../src/screens/metrics/usage-model.tsx");

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
  money: { settings: { main_number: "tokens", prices: "off" }, shown: "tokens", as_of: null, cost_nanos: 0, unpriced_tokens: 0 },
};
const metrics = {
  running: 2, tasks_waiting: 1, tasks_done: 6, tasks_failed: 0, merges: 19, merges_into_main: 5, merges_undone: 1, bucket_ms: 2 * HOUR, models: [], activity: [], activity_since_ms: null, tools: [], tools_since_ms: null,
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
const TITLES = { project: "By project", agent: "By agent", run: "By run", account: "By account", harness: "By harness" };
const panel = (markup, title) => {
  const start = markup.indexOf(`<h2>${title}</h2>`);
  assert.ok(start >= 0, `no ${title} panel`);
  return markup.slice(start, markup.indexOf("</section>", start));
};
const tableText = (markup, by = "project") => {
  const part = panel(markup, TITLES[by]);
  return text(part.slice(part.indexOf("<table")));
};
const usageRow = (key, value, patch = {}) => ({ key, tokens: value, runs: 1, last_ms: NOW - MINUTE, ...patch });

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
  const table = tableText(markup);
  assert.match(table, /^ Project Tokens Input Cache write Output Cache read Agents Runs Merges Last used Share of tokens /, "By project counts each project's merges, as the mockup does");
  assert.match(table, /Alpha 1\.4K 700 500 200 3K 2 3 4 61%/);
  assert.match(table, /Beta 900 500 300 100 1K 1 1 0 39%/);
});

test("across all projects, By agent and By run add a Project column, and an agent names its harness and accounts", () => {
  const agents = tableText(render(), "agent");
  assert.match(agents, /^ Agent Project Tokens Input Cache write Output Cache read Harness Account Runs Last used Share of tokens /);
  assert.match(agents, /Juniper Alpha 1\.4K 700 500 200 3K Codex Pool A, Claude 1, acct-b 61%/);
  assert.match(agents, /Master, Beta Beta 900 500 300 100 1K Claude Code Pool A, Claude 1 39%/);
  assert.doesNotMatch(agents, /graphify|ctx_\*/);
  const runs = tableText(render(), "run");
  assert.match(runs, /^ Run Project Tokens Input Cache write Output Cache read Agent Ended Result Share of tokens /);
  assert.match(runs, /#142 Calendar API endpoints Alpha 1\.4K 700 500 200 3K Juniper running Working 61%/);
  assert.match(runs, /No task Beta 900 500 300 100 1K Master, Beta 11 min ago 39%/);
});

test("By account and By harness name the projects that used each one", () => {
  assert.match(tableText(render(), "account"), /^ Account Tokens Input Cache write Output Cache read Agents Projects Runs Last used Share of tokens Pool A, Claude 1 2\.3K 1\.2K 800 300 4K 2 Alpha, Beta 100%/);
  const harnesses = tableText(render(), "harness");
  assert.match(harnesses, /^ Harness Tokens Input Cache write Output Cache read Agents Projects Share of tokens /);
  assert.match(harnesses, /Codex 1\.4K 700 500 200 3K 1 Alpha 61%/);
  assert.match(harnesses, /Claude Code 900 500 300 100 1K 1 Beta 39%/);
});

test("a picked project shows its name, stays picked and drops the Project column", () => {
  const markup = render({ projectId: "alpha" });
  assert.match(text(markup), /Alpha · Updated 10 s ago · provider-reported usage per turn/);
  assert.match(markup, /<option value="alpha" selected="">Alpha<\/option>/);
  assert.match(tableText(markup, "agent"), /^ Agent Tokens Input Cache write Output Cache read Harness Account Runs Last used Share of tokens /);
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
  const shown = (by) => tableText(render({ metrics: { kind: "ok", value: capped } }), by);
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
  const shown = (by) => tableText(render({ names: { agents: new Map(), accounts: names.accounts }, metrics: { kind: "ok", value: labelled } }), by);
  assert.match(shown("agent"), /Ada \(@calendar\) Alpha 1\.4K/);
  assert.match(shown("agent"), /Master Beta 900/);
  assert.match(shown("run"), /#142 Calendar API endpoints Alpha 1\.4K 700 500 200 3K Ada \(@calendar\) /);
});

test("across all projects By project and By agent total the Tokens tile, an Other row holding what the listed rows leave", () => {
  const listed = { ...metrics, projects: [metrics.projects[0]], agents: [metrics.agents[0]] };
  const shown = (by) => tableText(render({ metrics: { kind: "ok", value: listed } }), by);
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

test("the app page says what it counts and the engine's counting notice, only while the engine has one", () => {
  const words = text(render(withUsage({ counting_notice: "These numbers still use the old counting." })));
  assert.match(words, /These numbers still use the old counting\./);
  assert.doesNotMatch(text(render()), /old counting/);
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
  const words = text(render({ projectId: "alpha", ...withUsage({ limits: [limitsFor()], counting_notice: "These numbers still use the old counting." }) }));
  assert.match(words, /These numbers still use the old counting\./);
  assert.match(words, /Account limits/);
  assert.match(words, /Set reset time/);
});

test("the page has a panel by project, agent, run, account, pool and harness, then lines changed, in the mockup's order", () => {
  const markup = render(withUsage({ pools: [usageRow("pool-a", ALPHA)], days: [{ day_ms: SINCE, tokens: ALPHA }] }));
  const titles = [...markup.matchAll(/<h2>([^<]*)<\/h2>/gu)].map((match) => match[1]);
  assert.deepEqual(titles.filter((title) => /^(By|Lines)/u.test(title)), ["By project", "By agent", "By run", "By account", "By pool", "By harness", "Lines changed by project", "By day"]);
});

test("a panel shows at most five rows, and All columns holds every row with every column", () => {
  const many = Array.from({ length: 7 }, (_, index) => ({ key: `p${index}`, tokens: ALPHA, agents: 1, runs: 1, projects: [`p${index}`], merges: 0 }));
  const part = panel(render({ metrics: { kind: "ok", value: { ...metrics, projects: many } } }), "By project");
  assert.equal([...part.matchAll(/class="mx-row"/gu)].length, 5);
  assert.match(part, /<summary>All columns, 7 rows<\/summary>/);
  assert.equal([...part.slice(part.indexOf("<table")).matchAll(/<th scope="row">p\d/gu)].length, 7);
});

test("By account keeps Runs and Last used from the usage read", () => {
  const part = text(panel(render(withUsage({ accounts: [usageRow("acct-a", ALPHA, { runs: 9, last_ms: NOW - 3 * MINUTE })] })), "By account"));
  assert.match(part, /Runs Last used/);
  assert.match(part, /9 3 min ago/);
});

test("By pool names the pool and a harness that reported nothing says so, never zero", () => {
  const limits = { ...LIMITS, pools: new Map([["pool-a", "Pool A"]]) };
  const silent = usageRow("pool-a", tokens(0, 0, 0, 0), { not_reported_by: "rcli" });
  const part = text(panel(render({ ...withUsage({ pools: [silent] }), filter: { ...FILTER, limits } }), "By pool"));
  assert.match(part, /Pool A/);
  assert.match(part, /Usage not reported by rcli/);
});

test("an agent only the usage read knows stays listed as not reported", () => {
  const silent = usageRow("team-44", tokens(0, 0, 0, 0), { not_reported_by: "rcli" });
  const part = text(panel(render(withUsage({ agents: [silent] })), "By agent"));
  assert.match(part, /Agent 44 Usage not reported by rcli/);
  assert.doesNotMatch(part, /Agent 44 Usage not reported by rcli 0 cache read/, "a harness that reported nothing never reads as zero");
});

test("runs tied to no project read as Unattributed", () => {
  const rows = [{ key: "", tokens: ALPHA, agents: 0, runs: 1, projects: [], merges: 0 }];
  assert.match(text(panel(render({ metrics: { kind: "ok", value: { ...metrics, projects: rows } } }), "By project")), /Unattributed/);
});

test("Lines changed lists added and removed, and All columns keeps change sets and accepted commits", () => {
  const part = text(panel(render(), "Lines changed by project"));
  assert.match(part, /Alpha \+90 −57/);
  assert.match(part, /Project Change sets Lines Accepted commits Lines accepted/);
});

test("By day shows the newest seven days and offers the rest", () => {
  const days = Array.from({ length: 30 }, (_, index) => ({ day_ms: SINCE + (index - 29) * DAY, tokens: ALPHA }));
  const part = panel(render(withUsage({ days })), "By day");
  const shown = [...part.matchAll(/<th scope="row">([^<]*)<\/th>/gu)].map((match) => match[1]);
  assert.deepEqual(shown, [0, 1, 2, 3, 4, 5, 6].map((back) => dayLabel(SINCE - back * DAY, 0)));
  assert.match(text(part), /Show all 30 days/);
  assert.match(text(part), /Day Tokens Input Cache write Output Cache read/);
});

test("a range with no usage shows the empty state and keeps Account limits", () => {
  const words = text(render(withUsage({ total: tokens(0, 0, 0, 0), lines: [], days: [] })));
  assert.match(words, /No usage in this range Runs show up here a few seconds after an agent works\./);
  assert.match(words, /Account limits/);
});

test("a row with no tokens draws no bar, so it never reads as a little usage", () => {
  const silent = usageRow("team-44", tokens(0, 0, 0, 0), { not_reported_by: "rcli" });
  const part = panel(render(withUsage({ agents: [silent] })), "By agent");
  const bar = part.slice(part.indexOf("Agent 44")).match(/<i style="width:([^"]*)"/);
  assert.ok(bar, "the row has its bar track");
  assert.equal(bar[1], "0%");
});

const NOTHING = tokens(0, 0, 0, 0);
const agentRow = (project, agent, value) => ({ project, agent, harnesses: ["codex"], accounts: ["acct-a"], tokens: value });
const agentsRead = (rows, used) => ({ ...withUsage({ agents: used }), metrics: { kind: "ok", value: { ...metrics, agents: rows } } });

test("one agent key used in two projects keeps each project's own Runs and Last used (METRICS-AGENT-RUNS)", () => {
  const rows = [agentRow("alpha", "onboarding", ALPHA), agentRow("beta", "onboarding", BETA)];
  const used = [
    usageRow("onboarding", ALPHA, { project: "alpha", runs: 42, last_ms: NOW - 5 * MINUTE }),
    usageRow("onboarding", BETA, { project: "beta", runs: 7, last_ms: NOW - 3 * MINUTE }),
  ];
  const part = panel(render(agentsRead(rows, used)), "By agent");
  const lines = [...part.slice(part.indexOf("<table")).matchAll(/<tr[^>]*>.*?<\/tr>/gsu)].map((match) => text(match[0]));
  const of = (project) => lines.find((line) => line.includes(`Onboarding ${project} `)) ?? "";
  assert.match(of("Alpha"), / 42 5 min ago /, "alpha's row shows alpha's runs and last use");
  assert.match(of("Beta"), / 7 3 min ago /, "beta's row shows beta's runs and last use");
  assert.equal(lines.filter((line) => line.includes("Onboarding")).length, 2, "each project lists the key once");
});

test("an agent's Runs and Last used come from its own project's usage row, and an agent that reported nothing names its project (METRICS-AGENT-RUNS)", () => {
  const rows = [agentRow("alpha", "onboarding", BETA)];
  const used = [usageRow("onboarding", ALPHA, { project: "alpha", runs: 42, last_ms: NOW - 5 * MINUTE }), usageRow("team-44", NOTHING, { project: "beta", not_reported_by: "rcli" })];
  const part = text(panel(render(agentsRead(rows, used)), "By agent"));
  assert.match(part, / 42 5 min ago /);
  assert.match(part, /Agent 44 Beta /);
});

test("an agent on one project keeps its Runs and Last used, and one that reported nothing lists once (r1 L1)", () => {
  const rows = [agentRow("alpha", "team-8", ALPHA), agentRow("beta", "team-9", BETA)];
  const used = [usageRow("team-8", ALPHA, { project: "alpha", runs: 5, last_ms: NOW - 3 * MINUTE }), usageRow("team-9", BETA, { project: "beta", not_reported_by: "rcli" })];
  const markup = render(agentsRead(rows, used));
  assert.match(tableText(markup, "agent"), /5 3 min ago/);
  assert.equal([...panel(markup, "By agent").matchAll(/class="mx-row"/gu)].length, 2);
});

test("a usage row with no metrics row is listed only when its harness reported nothing (r1 L1)", () => {
  assert.doesNotMatch(text(panel(render(withUsage({ agents: [usageRow("team-44", ALPHA)] })), "By agent")), /Agent 44/);
});

test("a range with only lines changed is not empty (r1 L1)", () => {
  const words = text(render(withUsage({ total: NOTHING, days: [] })));
  assert.doesNotMatch(words, /No usage in this range/);
  assert.match(words, /Lines changed by project/);
});

test("a range where only a harness that reports nothing ran is not empty (r1 L1)", () => {
  assert.doesNotMatch(text(render(withUsage({ total: NOTHING, lines: [], days: [], not_reported_by: ["rcli"] }))), /No usage in this range/);
});

test("Lines changed shows at most five rows and All columns counts every one (r1 L1)", () => {
  const lines = Array.from({ length: 7 }, (_, index) => ({ project: `p${index}`, change_sets: 1, added: 9 - index, removed: 1, commits: 1, committed_added: 1, committed_removed: 1 }));
  const part = panel(render(withUsage({ lines })), "Lines changed by project");
  assert.equal([...part.matchAll(/class="mx-row"/gu)].length, 5);
  assert.match(part, /<summary>All columns, 7 rows<\/summary>/);
});

test("a row that reported nothing lines up with the table's columns (r1 L1)", () => {
  const silent = usageRow("team-44", NOTHING, { not_reported_by: "rcli" });
  const part = panel(render(withUsage({ agents: [silent] })), "By agent");
  const rows = [...part.slice(part.indexOf("<table")).matchAll(/<tr[^>]*>.*?<\/tr>/gsu)].map((match) => match[0]);
  const width = (row) => [...row.matchAll(/<t[dh]\b[^>]*>/gu)].reduce((sum, cell) => sum + Number(/colspan="(\d+)"/iu.exec(cell[0])?.[1] ?? 1), 0);
  const row = rows.find((one) => one.includes("Usage not reported by rcli"));
  assert.ok(row, "the row is in the table");
  assert.equal(width(row), width(rows[0]));
});
