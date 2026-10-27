import "./load-source.mjs";
import assert from "node:assert/strict";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";

const { AccountChoiceView, ProjectAccountOrderView, planFor, planLine, poolStrategy, projectStrategy, rankInput, reserveInput, strategyOptions, STRATEGIES } = await import("../src/screens/settings/account-choice.tsx");
const { plainError } = await import("../src/api.ts");

const schedule = {
  strategy: "manual_rank",
  pools: [{ id: "pool-main", strategy: "balanced" }],
  projects: [{ id: "project-a", strategy: "use_it_or_lose_it" }],
  accounts: [{ connection: "acct-1", plan: { rank: 2, reserve_percent: 10, pin: "use_last" } }],
};
const member = (id) => ({ id, kind: "claude", provider: "Claude", status: "signed_in", status_label: "Signed in" });
const pool = { id: "pool-main", name: "Main", summary: "2 Claude", members: [member("acct-1"), member("acct-2")], project_count: 1 };
const forbidden = () => { throw new Error("static rendering must not save"); };

test("rank and reserve fields accept whole numbers in range and refuse the rest", () => {
  assert.deepEqual(rankInput(""), { kind: "ok", value: null });
  assert.deepEqual(rankInput(" 7 "), { kind: "ok", value: 7 });
  assert.deepEqual(rankInput("999"), { kind: "ok", value: 999 });
  for (const bad of ["0", "1000", "-1", "2.5", "two"]) assert.equal(rankInput(bad).kind, "error", bad);
  assert.deepEqual(reserveInput(""), { kind: "ok", value: 0 });
  assert.deepEqual(reserveInput("15%"), { kind: "ok", value: 15 });
  assert.deepEqual(reserveInput("99"), { kind: "ok", value: 99 });
  for (const bad of ["100", "-5", "1.5", "half"]) assert.equal(reserveInput(bad).kind, "error", bad);
});

test("stored choices read back per pool, project and account with defaults for the rest", () => {
  assert.equal(poolStrategy(schedule, "pool-main"), "balanced");
  assert.equal(poolStrategy(schedule, "pool-other"), null);
  assert.equal(projectStrategy(schedule, "project-a"), "use_it_or_lose_it");
  assert.equal(projectStrategy(schedule, "project-b"), null);
  assert.deepEqual(planFor(schedule, "acct-1"), { rank: 2, reserve_percent: 10, pin: "use_last" });
  assert.deepEqual(planFor(schedule, "acct-2"), { rank: null, reserve_percent: 0, pin: null });
  assert.equal(planLine(planFor(schedule, "acct-1")), "Rank 2, keeps 10% of its week, used last");
  assert.equal(planLine(planFor(schedule, "acct-2")), "No rank, used to 100%");
  assert.equal(planLine({ rank: 1, reserve_percent: 0, pin: "never_use" }), "Never used");
  assert.deepEqual(STRATEGIES.map((item) => item.value), ["use_it_or_lose_it", "manual_rank", "balanced"]);
  assert.deepEqual(strategyOptions("Manual rank")[0], { value: "", label: "Default (Manual rank)" });
});

test("the panel shows the app strategy, the pool's own strategy and each member's plan", () => {
  const html = renderToStaticMarkup(createElement(AccountChoiceView, {
    schedule: { kind: "ok", value: schedule }, pools: { kind: "ok", value: [pool] },
    names: new Map([["acct-1", "Work Claude"], ["acct-2", "Home Claude"]]), onChange: forbidden, onRetry: forbidden,
  }));
  assert.match(html, /How every pool picks an account/);
  assert.match(html, /<option value="manual_rank" selected="">Manual rank<\/option>/);
  assert.match(html, /Main: account order/);
  assert.match(html, /<option value="balanced" selected="">Balanced<\/option>/);
  assert.match(html, /Work Claude/);
  assert.match(html, /Home Claude/);
  assert.match(html, /Rank 2, keeps 10% of its week, used last/);
  assert.match(html, /No rank, used to 100%/);
  assert.match(html, /<option value="never_use">Never use<\/option>/);
  const failed = renderToStaticMarkup(createElement(AccountChoiceView, {
    schedule: { kind: "error", code: "owner_read_failed", message: "x" }, pools: { kind: "ok", value: [pool] },
    names: new Map(), onChange: forbidden, onRetry: forbidden,
  }));
  assert.match(failed, /Couldn&#x27;t load account choice\./);
});

test("every scheduling refusal reads as a sentence", () => {
  for (const code of ["account_rank_invalid", "account_reserve_invalid", "account_schedule_invalid", "account_schedule_full", "connection_not_found", "pool_not_found"]) {
    assert.doesNotMatch(plainError(code), /Something went wrong/, code);
  }
});

test("a project shows its own account order, defaulting to its pool's strategy", () => {
  const html = renderToStaticMarkup(createElement(ProjectAccountOrderView, {
    schedule: { kind: "ok", value: schedule }, project: "project-b", pool: "pool-main", onChange: forbidden,
  }));
  assert.match(html, /Account order for this project/);
  assert.match(html, /<option value="" selected="">Default \(Balanced\)<\/option>/);
  const own = renderToStaticMarkup(createElement(ProjectAccountOrderView, {
    schedule: { kind: "ok", value: schedule }, project: "project-a", pool: null, onChange: forbidden,
  }));
  assert.match(own, /<option value="use_it_or_lose_it" selected="">Use it or lose it<\/option>/);
  assert.match(own, /Default \(Manual rank\)/);
});
