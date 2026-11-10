import "./load-source.mjs";
import assert from "node:assert/strict";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";

const { NextAccountsPanel, atText, standingLine } = await import("../src/screens/settings/account-choice.tsx");
const { MetricsPage } = await import("../src/screens/metrics/view.tsx");

const monday = new Date(2026, 9, 26, 8, 0).getTime();
const member = (id) => ({ id, kind: "claude", provider: "Claude", status: "signed_in", status_label: "Signed in" });
const pool = { id: "pool-main", name: "Main", summary: "3 Claude", members: [member("acct-1"), member("acct-2"), member("acct-3")], project_count: 1 };
const ranking = [{
  pool: "pool-main",
  strategy: "use_it_or_lose_it",
  accounts: [
    { connection: "acct-2", standing: { kind: "next", weekly_resets_ms: new Date(2026, 10, 2, 9, 0).getTime(), weekly_left_percent: 40, use_last: false } },
    { connection: "acct-1", standing: { kind: "reserve_reached", reserve_percent: 20, until_ms: null } },
    { connection: "acct-3", standing: { kind: "five_hour_cap", until_ms: new Date(2026, 9, 26, 14, 30).getTime() } },
  ],
}];
const names = new Map([["acct-1", "Work Claude"], ["acct-2", "Home Claude"], ["acct-3", "Spare Claude"]]);
const forbidden = () => { throw new Error("static rendering must not save"); };

test("a reset or pause time reads as a local clock, with the weekday when it is not today", () => {
  assert.equal(atText(new Date(2026, 9, 26, 14, 30).getTime(), monday), "14:30");
  assert.equal(atText(new Date(2026, 10, 2, 9, 0).getTime(), monday), "Mon 09:00");
  assert.equal(atText(new Date(2026, 9, 28, 7, 5).getTime(), monday), "Wed 07:05");
});

test("each standing reads as one short reason", () => {
  const at = new Date(2026, 9, 26, 14, 30).getTime();
  assert.equal(standingLine({ kind: "next", weekly_resets_ms: new Date(2026, 10, 2, 9, 0).getTime(), weekly_left_percent: 40, use_last: false }, monday), "resets Mon 09:00, 40% left");
  assert.equal(standingLine({ kind: "next", weekly_resets_ms: null, weekly_left_percent: 12, use_last: true }, monday), "12% left, used last");
  assert.equal(standingLine({ kind: "next", weekly_resets_ms: null, weekly_left_percent: null, use_last: false }, monday), "no usage read yet");
  assert.equal(standingLine({ kind: "reserve_reached", reserve_percent: 20, until_ms: at }, monday), "reserve reached (20%)");
  assert.equal(standingLine({ kind: "reserve_reached", reserve_percent: 0, until_ms: at }, monday), "weekly limit used up until 14:30");
  assert.equal(standingLine({ kind: "reserve_reached", reserve_percent: 0, until_ms: null }, monday), "weekly limit used up");
  assert.equal(standingLine({ kind: "five_hour_cap", until_ms: at }, monday), "5-hour cap until 14:30");
  assert.equal(standingLine({ kind: "five_hour_cap", until_ms: null }, monday), "5-hour cap reached");
  assert.equal(standingLine({ kind: "never_use" }, monday), "never use");
  assert.equal(standingLine({ kind: "paused", until_ms: at }, monday), "paused until 14:30");
});

test("the next accounts panel shows each pool's next accounts in order with why the rest wait", () => {
  const html = renderToStaticMarkup(createElement(NextAccountsPanel, { ranking: { kind: "ok", value: ranking }, pools: { kind: "ok", value: [pool] }, names, nowMs: monday }));
  assert.match(html, /Next accounts/);
  assert.match(html, /<li[^>]*>.*Home Claude.*resets Mon 09:00, 40% left/);
  assert.ok(html.indexOf("Home Claude</span><span class=\"small secondary\">resets") < html.indexOf("reserve reached (20%)"), html);
  assert.match(html, /Work Claude.*reserve reached \(20%\)/);
  assert.match(html, /Spare Claude.*5-hour cap until 14:30/);
  const failed = renderToStaticMarkup(createElement(NextAccountsPanel, { ranking: { kind: "error", code: "owner_read_failed", message: "x" }, pools: { kind: "ok", value: [pool] }, names, nowMs: monday }));
  assert.match(failed, /Couldn&#x27;t read the next accounts\./);
});

test("the Metrics page's account section carries the next accounts panel", () => {
  const panel = createElement(NextAccountsPanel, { ranking: { kind: "ok", value: ranking }, pools: { kind: "ok", value: [pool] }, names, nowMs: monday });
  const report = { total: { input: 0, output: 0, cache_read: 0, cache_write: 0 }, runs: 0, limits: [], projects: [], pools: [], accounts: [], agents: [], days: [], lines: [], partial_homes: 0 };
  const metrics = { running: 0, tasks_waiting: 0, tasks_done: 0, tasks_failed: 0, merges: 0, merges_into_main: 0, merges_undone: 0, bucket_ms: 86_400_000, series: [], runs: [], projects: [], agents: [], accounts: [], harnesses: [] };
  const limits = { pools: new Map(), failure: "", busy: null, next: panel, revision: 0, onRefresh: forbidden, onSetReset: forbidden };
  const html = renderToStaticMarkup(createElement(MetricsPage, {
    projectId: null, usage: { kind: "ok", value: report }, metrics: { kind: "ok", value: metrics }, adoption: null, range: "7d", by: "project",
    names: { agents: new Map(), accounts: new Map() }, sinceMs: monday - 7 * 86_400_000, nowMs: monday, updatedMs: monday, offset: 0,
    filter: { projects: [], onProject: forbidden, limits }, onRange: forbidden, onBy: forbidden, onRetry: forbidden,
  }));
  assert.ok(html.indexOf("Account limits") < html.indexOf("Next accounts"), html);
  assert.match(html, /Main/);
  assert.match(html, /Home Claude.*resets Mon 09:00, 40% left/);
});
