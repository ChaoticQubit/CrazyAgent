import "./load-source.mjs";
import assert from "node:assert/strict";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";

const view = await import("../src/screens/metrics/view.tsx");
const page = await import("../src/screens/project/index.tsx");
const router = await import("../src/ui/router.ts");

const MINUTE = 60_000;
const HOUR = 60 * MINUTE;
const DAY = 24 * HOUR;
const SINCE = Date.UTC(2026, 10, 7);
const NOW = SINCE + 9 * HOUR;
const forbidden = () => { throw new Error("static rendering must not call the engine"); };
const tokens = (patch = {}) => ({ input: 0, output: 0, cache_read: 0, cache_write: 0, ...patch });
const row = (key, patch = {}) => ({ key, runs: 1, last_ms: NOW - 5 * MINUTE, ...patch });
const limits = (patch = {}) => ({
  account: "acct-a", plan: "max", five_hour: { used_percent: 34.4, resets_at_ms: NOW + 2 * HOUR }, weekly: { used_percent: 81, resets_at_ms: NOW + 3 * DAY + 4 * HOUR },
  five_hour_reset_override_ms: null, weekly_reset_override_ms: null, source: "poll", observed_ms: NOW - 3 * MINUTE, checked_ms: NOW - 3 * MINUTE, failure: null,
  five_hour_left_out: null, weekly_left_out: null, five_hour_left_out_label: null, weekly_left_out_label: null, ...patch,
});
const usage = (patch = {}) => ({
  since_ms: SINCE, total: tokens({ input: 1200, output: 300, cache_read: 4000, cache_write: 800 }), runs: 3,
  projects: [], pools: [], accounts: [],
  agents: [
    row("team-8", { tokens: tokens({ input: 700, output: 200, cache_read: 3000, cache_write: 500 }) }),
    row("master-alpha", { tokens: tokens({ input: 500, output: 100, cache_read: 1000, cache_write: 300 }) }),
  ],
  days: [],
  lines: [{ project: "alpha", change_sets: 3, added: 90, removed: 57, commits: 2, committed_added: 80, committed_removed: 55 }],
  agent_lines: [], limits: [limits()], partial_homes: 0,
  money: { settings: { main_number: "tokens", prices: "off" }, shown: "tokens", as_of: null, cost_nanos: 0, unpriced_tokens: 0 }, ...patch,
});
const metrics = (patch = {}) => ({
  running: 2, tasks_waiting: 4, tasks_done: 5, tasks_failed: 1, merges: 12, merges_into_main: 3, merges_undone: 0, bucket_ms: 2 * HOUR,
  series: [
    { start_ms: SINCE, tokens: tokens({ input: 100, cache_write: 50, output: 20, cache_read: 999 }) },
    { start_ms: SINCE + 4 * HOUR, tokens: tokens({ input: 300, cache_write: 10, output: 40 }) },
  ],
  runs: [
    { project: "alpha", agent: "team-8", session: "rollout-2026-11-07-abc", task: 142, title: "Calendar API endpoints", status: "running", last_ms: NOW - 5 * MINUTE, tokens: tokens({ input: 700, output: 200, cache_read: 3000, cache_write: 500 }) },
    { project: "alpha", agent: "master-alpha", session: "m-1", task: null, title: null, status: null, last_ms: NOW - 11 * MINUTE, tokens: tokens({ input: 500, output: 100, cache_read: 1000, cache_write: 300 }) },
  ],
  accounts: [{ key: "acct-a", tokens: tokens({ input: 1200, output: 300, cache_read: 4000, cache_write: 800 }), agents: 2, runs: 2 }],
  agents: [
    { project: "alpha", agent: "team-8", label: "Juniper", harnesses: ["codex"], more_harnesses: 0, accounts: ["acct-a"], more_accounts: 0, tokens: tokens({ input: 700, output: 200, cache_read: 3000, cache_write: 500 }), turns: 12 },
    { project: "alpha", agent: "master-alpha", label: "Master", harnesses: ["claude-code"], more_harnesses: 0, accounts: ["acct-a"], more_accounts: 0, tokens: tokens({ input: 500, output: 100, cache_read: 1000, cache_write: 300 }), turns: 9 },
  ],
  harnesses: [
    { key: "codex", tokens: tokens({ input: 700, output: 200, cache_read: 3000, cache_write: 500 }), agents: 1, runs: 1 },
    { key: "claude-code", tokens: tokens({ input: 500, output: 100, cache_read: 1000, cache_write: 300 }), agents: 1, runs: 1 },
  ],
  ...patch,
});
const adoption = {
  nudges: "off", counted: [], skills: ["tdd", "code-review"],
  agents: [
    { agent: 8, role: "sub", harness: "codex", active: true, counts: [], skills: [3, 1], other_skills: [{ name: "brainstorming", count: 2 }], graphify: 44, context_mode: 37, flags: [], reminded: [] },
    { agent: 1, role: "master", harness: "claude-code", active: true, counts: [], skills: [0, 4], graphify: 58, context_mode: 41, flags: [], reminded: [] },
  ],
};
const names = {
  agents: new Map([["team-8", "Juniper · Codex"], ["master-alpha", "Master · Claude Code"]]),
  accounts: new Map([["acct-a", "Pool A, Claude 1"]]),
};
const render = (patch = {}) => renderToStaticMarkup(createElement(view.MetricsPage, {
  projectId: "alpha", usage: { kind: "ok", value: usage() }, metrics: { kind: "ok", value: metrics() }, adoption: { kind: "ok", value: adoption },
  range: "today", by: "agent", names, sinceMs: SINCE, nowMs: NOW, updatedMs: NOW - 10_000, offset: 0,
  onRange: forbidden, onBy: forbidden, onRetry: forbidden, ...patch,
}));
const text = (markup) => markup.replace(/<[^>]*>/gu, " ").replace(/&#x27;/gu, "'").replace(/&amp;/gu, "&").replace(/\s+/gu, " ");
const tableText = (markup) => text(markup.slice(markup.indexOf("<table")));

test("the Metrics tab's tiles show the headline with its three parts and the cache re-reads apart", () => {
  const shown = text(render());
  assert.match(shown, /Tokens 2\.3K 1\.2K input, 800 cache write, 300 output/);
  assert.match(shown, /Re-read from cache 4K 67% of everything read; billed far lower, not in Tokens/);
  assert.match(shown, /Runs 3 2 running now/);
  assert.match(shown, /Tasks 5 done 4 waiting, 1 failed/);
  assert.match(shown, /Lines \+90 −57 across 3 change sets/);
  assert.match(shown, /Tasks 5 done 4 waiting, 1 failed Merges 12 3 into main, 0 undone Lines /, "six tiles, Merges between Tasks and Lines");
  assert.equal((render().match(/class="mx-tile( quiet)?"/gu) ?? []).length, 6, "six tiles");
});

test("the project tab shows this project only, with no project picker", () => {
  const markup = render();
  assert.match(text(markup), /This project only · Updated 10 s ago · provider-reported usage per turn/);
  assert.doesNotMatch(markup, /<select|All projects/);
  assert.match(text(markup), /Today 7 days 30 days/);
});

test("the chart draws a stacked bar for every two hours since midnight and never draws cache re-reads", () => {
  const markup = render();
  const chart = markup.slice(markup.indexOf("<svg"), markup.indexOf("</svg>"));
  assert.match(chart, /aria-label="Tokens per two hours: input, cache write and output"/);
  for (const part of ["in", "cw", "out"]) assert.equal(chart.match(new RegExp(`<rect class="${part}"`, "gu")).length, 5, part);
  assert.deepEqual([...chart.matchAll(/<text class="ax"[^>]*text-anchor="middle"[^>]*>([^<]*)</gu)].map((match) => match[1]), ["00:00", "02:00", "04:00", "06:00", "08:00"]);
  assert.match(chart, /<title>00:00: 100 input<\/title>/);
  assert.doesNotMatch(chart, />[^<]*(999|cache read|re-read)[^<]*</iu);
  assert.match(text(markup), /Input Cache write Output Re-read from cache: 4K, not drawn/);
});

test("the chart fills a week with one bar a day and labels each by its date", () => {
  const since = SINCE - 6 * DAY;
  const bars = view.chartBars(metrics({ bucket_ms: DAY, series: [{ start_ms: since + 2 * DAY, tokens: tokens({ input: 5 }) }] }), since, NOW);
  assert.deepEqual(bars.map((bar) => (bar.start_ms - since) / DAY), [0, 1, 2, 3, 4, 5, 6]);
  assert.deepEqual(bars.map((bar) => bar.tokens.input), [0, 0, 5, 0, 0, 0, 0]);
  assert.equal(view.barLabel(SINCE, DAY, 0), new Date(SINCE).toLocaleDateString(undefined, { timeZone: "UTC", month: "short", day: "numeric" }));
  assert.equal(view.barLabel(SINCE + 14 * HOUR, 2 * HOUR, 60), "15:00");
  assert.throws(() => view.chartBars(metrics({ bucket_ms: 0 }), SINCE, NOW), /a chart bar spans time/);
});

test("By agent lists each agent's tokens in four parts, its turns in the range, its tool counts and its share, with a total", () => {
  const markup = render();
  const table = tableText(markup);
  assert.match(table, /^ Agent Tokens Input Cache write Output Cache read Turns graphify ctx_\* Skills Share of tokens /);
  assert.match(table, /Juniper · Codex 1\.4K 700 500 200 3K 12 44 37 6 61%/);
  assert.match(table, /Master · Claude Code 900 500 300 100 1K 9 58 41 4 39%/);
  assert.match(table, /Total 2\.3K 1\.2K 800 300 4K cache read 67% of all read/);
  assert.match(markup, /<th scope="col" class="num muted">Cache read<\/th>/);
});

test("By agent leaves Turns empty for an agent the engine gives no turns: a row no team agent owns, or a reply from an engine before turns were kept", () => {
  const [juniper, master] = metrics().agents;
  for (const agents of [[juniper, { ...master, turns: null }], [juniper, (({ turns, ...older }) => older)(master)], [juniper]]) {
    const table = tableText(render({ metrics: { kind: "ok", value: metrics({ agents }) } }));
    assert.match(table, /Juniper · Codex 1\.4K 700 500 200 3K 12 44 37 6 61%/);
    assert.match(table, /Master · Claude Code 900 500 300 100 1K 58 41 4 39%/, JSON.stringify(agents));
  }
});

test("By run titles each run by its task and shows its agent, end and result", () => {
  const table = tableText(render({ by: "run" }));
  assert.match(table, /^ Run Tokens Input Cache write Output Cache read Agent Ended Result Share of tokens /);
  assert.match(table, /#142 Calendar API endpoints 1\.4K 700 500 200 3K Juniper · Codex running Working 61%/);
  assert.match(table, /No task 900 500 300 100 1K Master · Claude Code 11 min ago 39%/);
});

test("By account shows its agents, the limit with less room left and when that resets", () => {
  const table = tableText(render({ by: "account" }));
  const resets = new Date(NOW + 3 * DAY + 4 * HOUR).toLocaleDateString(undefined, { month: "short", day: "numeric" });
  assert.match(table, /^ Account Tokens Input Cache write Output Cache read Agents Limit left Resets Share of tokens /);
  assert.match(table, new RegExp(`Pool A, Claude 1 2\\.3K 1\\.2K 800 300 4K 2 19% ${resets} 100%`));
  assert.deepEqual(view.limitLeft(limits({ weekly: null }), NOW), { left: "66%", resets: "in 2 h" });
  assert.deepEqual(view.limitLeft(undefined, NOW), { left: "", resets: "" });
});

test("Total is the Tokens tile and every share is out of it; an Other row holds what a capped list leaves out", () => {
  const capped = metrics({ runs: [metrics().runs[0]] });
  const table = tableText(render({ by: "run", metrics: { kind: "ok", value: capped } }));
  assert.match(table, /#142 Calendar API endpoints 1\.4K 700 500 200 3K Juniper · Codex running Working 61%/);
  assert.match(table, /Other, not listed 900 500 300 100 1K 39%/);
  assert.match(table, /Total 2\.3K 1\.2K 800 300 4K cache read 67% of all read/);
  const full = tableText(render({ by: "run" }));
  assert.doesNotMatch(full, /Other, not listed/);
  assert.match(full, /Total 2\.3K 1\.2K 800 300 4K/);
  const agents = tableText(render({ usage: { kind: "ok", value: usage({ agents: [usage().agents[1]] }) } }));
  assert.match(agents, /Master · Claude Code 900 500 300 100 1K 9 58 41 4 39%/);
  assert.match(agents, /Other, not listed 1\.4K 700 500 200 3K 61%/);
});

test("By harness names each harness with its agents and runs", () => {
  const table = tableText(render({ by: "harness" }));
  assert.match(table, /^ Harness Tokens Input Cache write Output Cache read Agents Runs Share of tokens /);
  assert.match(table, /Codex 1\.4K 700 500 200 3K 1 1 61%/);
  assert.match(table, /Claude Code 900 500 300 100 1K 1 1 39%/);
});

test("the project page lists Metrics after Process, and its route opens it", () => {
  const ids = page.TABS.map((tab) => tab.id);
  assert.equal(ids.indexOf("metrics"), ids.indexOf("process") + 1);
  assert.equal(page.TABS.find((tab) => tab.id === "metrics").icon, "trending-up");
  assert.deepEqual(router.parseRoute("#/projects/alpha/metrics"), { name: "project", id: "alpha", tab: "metrics" });
});

test("both Metrics views open on 7 days, as the mockup does", () => {
  for (const props of [{ projectId: "p" }, { projectId: null, filter: { projects: [], onProject: forbidden } }]) {
    const markup = renderToStaticMarkup(createElement(view.MetricsView, props));
    assert.match(markup, /aria-checked="true"[^>]*>7 days</, JSON.stringify(props));
  }
});

test("one project on the app page is named in the singular", () => {
  const markup = render({ projectId: null, filter: { projects: [{ id: "alpha", name: "Alpha" }], onProject: forbidden } });
  assert.match(text(markup), /1 project · Updated/u);
});

const LABEL = "Estimated cost at API prices";
const bundled = (patch = {}) => ({ settings: { main_number: "dollars", prices: "bundled" }, shown: "dollars", as_of: "2026-11-08", cost_nanos: 5_000_000_000, unpriced_tokens: 0, ...patch });
const tokensLead = () => bundled({ settings: { main_number: "tokens", prices: "bundled" }, shown: "tokens" });
const off = () => ({ settings: { main_number: "dollars", prices: "off" }, shown: "tokens", as_of: null, cost_nanos: 0, unpriced_tokens: 0 });
const priced = (money) => render({
  usage: { kind: "ok", value: usage({ money, agents: usage().agents.map((one) => ({ ...one, cost_nanos: 2_000_000_000 })) }) },
  metrics: { kind: "ok", value: metrics({ series: metrics().series.map((bar) => ({ ...bar, cost_nanos: 1_000_000_000 })) }) },
  notices: "the notice text",
});

test("a chart bar keeps its dollars, an empty bar has none", () => {
  const bars = view.chartBars(metrics({ bucket_ms: HOUR, series: [{ start_ms: SINCE, tokens: tokens({ input: 1 }), cost_nanos: 7 }] }), SINCE, SINCE + HOUR);
  assert.deepEqual(bars.map((bar) => bar.cost_nanos), [7, 0]);
});

test("dollars lead the first tile, the chart and the table, each with its label", () => {
  const markup = priced(bundled());
  assert.match(markup, new RegExp(`<span class="mx-tile-key">${LABEL}</span><span class="mx-tile-value">\\$5\\.00</span>`, "u"));
  assert.match(markup, new RegExp(`${LABEL} over time`, "u"));
  assert.match(markup, new RegExp(`<title>[^<]*\\$1\\.00 \\(${LABEL}, prices as of 2026-11-08\\)</title>`, "u"));
  assert.match(markup, new RegExp(`<th scope="col" class="num">${LABEL}</th>`, "u"));
  assert.match(markup, new RegExp(`title="${LABEL}, prices as of 2026-11-08"><b>\\$2\\.00</b>`, "u"));
  assert.doesNotMatch(markup, /mx-legend/u);
});

test("tokens lead when chosen and the dollars beside them still carry the label", () => {
  const markup = priced(tokensLead());
  assert.match(markup, /<span class="mx-tile-key">Tokens<\/span><span class="mx-tile-value">2\.3K<\/span>/u);
  assert.match(markup, new RegExp(`<div class="mx-tile" title="${LABEL}, prices as of 2026-11-08"><span class="mx-tile-key">Tokens</span>`, "u"));
  assert.match(markup, /<span class="mx-tile-detail">\$5\.00 · /u);
  assert.match(markup, /Tokens over time/u);
  assert.match(markup, /mx-legend/u);
  assert.match(markup, new RegExp(`title="${LABEL}, prices as of 2026-11-08"><b>[0-9.]+K?</b><small> \\$2\\.00</small>`, "u"));
  assert.match(markup, /aria-label="Tokens per two hours/u);
});

test("the chart names its dollars for a screen reader when dollars lead", () => {
  assert.match(priced(bundled()), new RegExp(`aria-label="${LABEL} per two hours"`, "u"));
});

test("with prices off the page has no dollar, no label and no main number switch", () => {
  const markup = priced(off());
  assert.doesNotMatch(markup, /\$\d/u);
  assert.doesNotMatch(markup, new RegExp(LABEL, "u"));
  assert.doesNotMatch(markup, /Prices as of/u);
  assert.doesNotMatch(markup, /aria-label="Main number"/u);
  assert.match(markup, /aria-label="Prices"/u);
});

test("both money switches show while prices are on", () => {
  const markup = priced(bundled());
  assert.match(markup, /aria-label="Main number"/u);
  assert.match(markup, /aria-label="Prices"/u);
});

test("the page says the price date, what has no price and credits CodeBurn", () => {
  const markup = priced(bundled({ unpriced_tokens: 1200 }));
  assert.match(markup, /Prices as of 2026-11-08\. Off hides every dollar\. Tokens keep counting\./u);
  assert.match(markup, /1\.2K tokens have no price, so they are left out of the dollars\./u);
  assert.match(markup, /<details class="mx-credit"><summary>Counting rules adapted from CodeBurn \(MIT\)<\/summary><pre>the notice text<\/pre><\/details>/u);
});

test("an Other row holds the dollars a capped list leaves out", () => {
  const value = usage({ money: bundled(), agents: [{ ...usage().agents[0], cost_nanos: 2_000_000_000 }] });
  const markup = render({ usage: { kind: "ok", value } });
  assert.match(tableText(markup), /Other, not listed \$3\.00/u);
});

test("a chart of under a cent repeats no axis figure", () => {
  const shown = render({
    usage: { kind: "ok", value: usage({ money: bundled() }) },
    metrics: { kind: "ok", value: metrics({ series: metrics().series.map((bar) => ({ ...bar, cost_nanos: 5_000_000 })) }) },
  });
  const figures = [...shown.matchAll(/class="ax"[^>]*>([^<]*)</gu)].map((found) => found[1]).filter((figure) => figure.includes("$"));
  assert.deepEqual(figures, ["&lt;$0.01"]);
});
