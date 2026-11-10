import "../load-source.mjs";
import assert from "node:assert/strict";
import { test } from "node:test";
import { TRIAL3_STEPS } from "./trial3-steps.mjs";
import { limitsVerdict, orderVerdict, sameOrder } from "./trial4-usage.mjs";
import { checks as usage } from "./trial4/n-d16-usage-limits.mjs";

const HOUR = 3600000;
const window = (label, fields = {}) => ({ label, used: "42% used", meter: true, reset: "Resets in 2 h 14 min", ...fields });
const card = (five = {}, weekly = {}) => ({ windows: [window("5-hour", five), window("Weekly", weekly)] });
const next = (connection, weekly_resets_ms, fields = {}) => ({ connection, standing: { kind: "next", weekly_resets_ms, weekly_left_percent: 40, use_last: false, ...fields } });
const pool = (accounts, strategy = "use_it_or_lose_it") => ({ pool: "pool-main", strategy, accounts });

test("the usage check holds one check of the trial 4 shape, run at the end", () => {
  assert.equal(usage.length, 1);
  const [check] = usage;
  assert.deepEqual(Object.keys(check).sort(), ["after", "id", "op", "run", "title", "why"]);
  assert.deepEqual([check.id, check.op, check.after], ["N-D16", ["OP-17"], "end"]);
  assert.ok(check.why.startsWith("owner: "));
  assert.ok(check.after === "end" || TRIAL3_STEPS.some((step) => step.slug === check.after));
  assert.equal(typeof check.run, "function");
});

test("every account showing its 5-hour and weekly use, meter and reset passes and any gap names its place", () => {
  assert.equal(limitsVerdict([card(), card({ reset: "Resets in 3 d 4 h (your time)" }, { used: "100% used", reset: "Resets in under a minute" })]),
    "2 accounts each show their 5-hour and weekly use and reset time");
  assert.throws(() => limitsVerdict([]), /Metrics shows no account limits/);
  assert.throws(() => limitsVerdict([card(), card({}, { used: "Not reported", reset: "The provider hasn't reported this window." })]),
    /account 2 of 2 Weekly: shows "Not reported", not how much is used/);
  assert.throws(() => limitsVerdict([card({ reset: "Next reset not known yet" })]), /account 1 of 1 5-hour: shows "Next reset not known yet", not when it resets/);
  assert.throws(() => limitsVerdict([card({ meter: false })]), /account 1 of 1 5-hour: shows no use meter/);
  assert.throws(() => limitsVerdict([{ windows: [window("Weekly")] }]), /account 1 of 1 shows the windows \["Weekly"\], not 5-hour and Weekly/);
});

test("an API-key account's card is a valid reading, while an unreadable sign-in or only API-key accounts are not", async () => {
  const client = await import("../../src/api-usage.ts");
  const unread = { used: "Not reported", meter: false, reset: "The provider hasn't reported this window." };
  const keyed = { notice: client.pollFailure("native_profile_codex_api_key"), ...card(unread, unread) };
  assert.equal(limitsVerdict([card(), keyed, card()]),
    "2 accounts each show their 5-hour and weekly use and reset time; 1 API-key accounts have no 5-hour or weekly window");
  assert.throws(() => limitsVerdict([card(), { ...keyed, notice: client.pollFailure("native_profile_codex_token_unreadable") }]),
    /account 2 of 2 5-hour: shows "Not reported", not how much is used/);
  assert.throws(() => limitsVerdict([keyed]), /not exercised: every one of the 1 accounts is an API-key account/);
});

test("a Use it or lose it pool passes when the soonest weekly reset is tried first and fails when a sooner one waits", () => {
  const now = 1_800_000_000_000;
  const ordered = pool([
    next("acct-soon", now + HOUR),
    next("acct-later", now + 30 * HOUR),
    next("acct-unknown", null),
    next("acct-spent", now, { weekly_left_percent: 0 }),
    next("acct-last", now - HOUR, { use_last: true }),
    { connection: "acct-paused", standing: { kind: "paused", until_ms: now + HOUR } },
  ]);
  const seen = orderVerdict([ordered, pool([next("acct-x", now)], "manual_rank")]);
  assert.deepEqual(seen.pools, [{ pool: "pool-main", next: ["acct-soon", "acct-later", "acct-unknown", "acct-spent", "acct-last"] }]);
  assert.equal(seen.detail, "1 of 1 Use it or lose it pools try the soonest weekly reset first (2 accounts with a known reset)");
  assert.throws(() => orderVerdict([pool([next("acct-later", now + 30 * HOUR), next("acct-soon", now + HOUR)])]),
    /Use it or lose it pool 1 of 1 tries the account at place 2, whose weekly limit resets 1740 min sooner, after the one at place 1/);
  assert.throws(() => orderVerdict([pool([next("acct-unknown", null), next("acct-soon", now), next("acct-later", now + HOUR)])]),
    /tries the account at place 2, whose weekly limit resets at a known time, after the one at place 1/);
});

test("the order is not exercised without a Use it or lose it pool holding two known weekly resets", () => {
  const now = 1_800_000_000_000;
  assert.throws(() => orderVerdict([pool([next("a", now), next("b", now + HOUR)], "balanced")]), /not exercised: none of the 1 pools picks accounts by Use it or lose it/);
  assert.throws(() => orderVerdict([pool([next("a", now), next("b", null), next("c", now - HOUR, { use_last: true })])]),
    /not exercised: none of the 1 Use it or lose it pools has two accounts to try next with a known weekly reset/);
});

test("the Next accounts list matches only the engine's names in the engine's order", () => {
  assert.equal(sameOrder(["Work", "Home"], ["Work", "Home"]), true);
  assert.equal(sameOrder(["Work", "Home"], ["Home", "Work"]), false);
  assert.equal(sameOrder(["Work", "Home"], ["Work"]), false);
});
