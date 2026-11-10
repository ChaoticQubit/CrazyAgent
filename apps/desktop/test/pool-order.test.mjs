import "./load-source.mjs";
import assert from "node:assert/strict";
import test from "node:test";

const { HELD_HEAD, heldLine, holdLine, isNear, orderLine, usageBars, whyLine } = await import("../src/screens/settings/pool-order.ts");
const { atText } = await import("../src/screens/settings/account-choice.tsx");

const NOW = new Date(2026, 10, 9, 12, 0).getTime();
const LATER = NOW + 90 * 60000;
const AT = atText(LATER, NOW);

const WHY = [
  [{ kind: "use_last" }, "Pinned to Use last, so it goes after accounts that are not."],
  [{ kind: "limit_reported" }, "A run just reported it at a limit, so it goes after accounts that have room."],
  [{ kind: "no_reading" }, "Its weekly use or reset is not known yet, so it goes after accounts where it is."],
  [{ kind: "resets_soonest", resets_ms: LATER }, `Its weekly limit resets soonest, ${AT}.`],
  [{ kind: "resets_later", resets_ms: LATER }, `Its weekly limit resets later, ${AT}.`],
  [{ kind: "least_used" }, "Least weekly use of the accounts ready now."],
  [{ kind: "more_used" }, "More weekly use than the one above."],
  [{ kind: "more_five_hour_used" }, "Same weekly use as the one above, more of its 5 hours used."],
  [{ kind: "ranked", rank: 3 }, "You ranked it 3."],
  [{ kind: "unranked" }, "No rank, so it goes after ranked accounts."],
  [{ kind: "tied_rank", rank: 2 }, "Same as the one above, and you ranked it 2."],
  [{ kind: "tied_rank", rank: null }, "Same as the one above, and it has no rank."],
  [{ kind: "tied_order" }, "Same as the one above, so it keeps its pool order."],
];

const HELD = [
  [{ kind: "not_signed_in" }, "Not signed in. Agents skip it until you sign in again."],
  [{ kind: "sign_in_unconfirmed" }, "Its sign-in needs a check before agents can use it."],
  [{ kind: "paused", until_ms: LATER }, `Paused until ${AT}`],
  [{ kind: "never_use" }, "Never use. You pinned it."],
  [{ kind: "five_hour_cap", until_ms: LATER }, `5-hour cap until ${AT}`],
  [{ kind: "five_hour_cap", until_ms: null }, "5-hour cap reached"],
  [{ kind: "reserve_reached", reserve_percent: 15, until_ms: LATER }, "Reserve reached (15%)"],
  [{ kind: "reserve_reached", reserve_percent: 0, until_ms: LATER }, `Weekly limit used up until ${AT}`],
  [{ kind: "reserve_reached", reserve_percent: 0, until_ms: null }, "Weekly limit used up"],
];

/** An account's limits with no reading, changed by the fields given. Input: the fields. Output: the limits. */
function limits(fields) {
  return {
    account: "a", plan: null, five_hour: null, weekly: null, five_hour_reset_override_ms: null, weekly_reset_override_ms: null,
    source: "poll", observed_ms: NOW, checked_ms: NOW, failure: null, five_hour_left_out: null, weekly_left_out: null,
    five_hour_left_out_label: null, weekly_left_out_label: null, ...fields,
  };
}

const used = (used_percent, resets_at_ms = LATER) => ({ used_percent, resets_at_ms });

for (const [why, line] of WHY) {
  test(`the order reason ${JSON.stringify(why)} reads as its sentence`, () => assert.equal(whyLine(why, NOW), line));
}

for (const [standing, line] of HELD) {
  test(`the held reason ${JSON.stringify(standing)} reads as its sentence`, () => assert.equal(heldLine(standing, NOW), line));
}

test("a project's hold on an account reads as which project holds it and until when", () => {
  assert.equal(holdLine({ project: "project-a", name: "Meals for two", until_ms: LATER }, NOW), `Held for Meals for two until ${AT}`);
});

test("the held list says what it is", () => {
  assert.equal(HELD_HEAD, "Held now, in pool order. Each one rejoins the order when its reason ends.");
});

test("a reason outside the table and a ready account as held are refused, never shown as blank", () => {
  assert.throws(() => whyLine({ kind: "because" }, NOW), /no words for the order reason because/);
  assert.throws(() => heldLine({ kind: "next", weekly_resets_ms: null, weekly_left_percent: null, use_last: false, why: { kind: "tied_order" } }, NOW), /a ready account is not held/);
});

test("the order line names the strategy, where it was set and how a row moves", () => {
  assert.equal(orderLine("use_it_or_lose_it", true, 0), "Order comes from Use it or lose it, this pool: the account whose weekly limit resets soonest goes first, then your rank. It follows live usage, so rows cannot be dragged.");
  assert.equal(orderLine("manual_rank", false, 0), "Order comes from Manual rank, the default for every pool: accounts go in the order you rank them. Set a rank under Schedule to move a row. Rows cannot be dragged.");
  assert.equal(orderLine("balanced", true, 0), "Order comes from Balanced, this pool: the account with the least weekly use goes first. Rank only breaks ties. It follows live usage, so rows cannot be dragged.");
});

test("the order line counts the projects that set their own order, one and many", () => {
  assert.match(orderLine("balanced", true, 1), / 1 project using this pool sets its own order\.$/);
  assert.match(orderLine("balanced", true, 4), / 4 projects using this pool set their own order\.$/);
  assert.doesNotMatch(orderLine("balanced", true, 0), /own order/);
  assert.throws(() => orderLine("balanced", true, -1), /a project count is a whole number from 0/);
  assert.throws(() => orderLine("fastest", true, 0), /no words for the order fastest/);
});

test("an account's usage bars are weekly then 5-hour, with what each has used and when it ends", () => {
  assert.deepEqual(usageBars(limits({ weekly: used(42.4), five_hour: used(10) }), NOW), [
    { name: "Weekly", percent: 42.4, text: `42% used, resets ${AT}`, tone: "ok" },
    { name: "5-hour", percent: 10, text: `10% used, until ${AT}`, tone: "ok" },
  ]);
  assert.deepEqual(usageBars(limits({ weekly: used(5, null), five_hour: used(6, null) }), NOW).map((bar) => bar.text), ["5% used", "6% used"]);
  assert.deepEqual(usageBars(limits({}), NOW), []);
});

test("a usage bar turns near at 80, full at 100, and never runs past its track", () => {
  const tone = (percent) => usageBars(limits({ weekly: used(percent) }), NOW)[0];
  assert.equal(tone(79).tone, "ok");
  assert.equal(tone(80).tone, "near");
  assert.equal(tone(99.9).tone, "near");
  assert.equal(tone(100).tone, "full");
  assert.equal(tone(120).percent, 100);
});

test("a usage bar reads a window as it stands now: a passed reset is empty and the owner's reset counts", () => {
  assert.deepEqual(usageBars(limits({ weekly: used(90, NOW - 1) }), NOW), [{ name: "Weekly", percent: 0, text: "0% used", tone: "ok" }]);
  const moved = NOW + 3 * 3600000;
  assert.equal(usageBars(limits({ weekly: used(90), weekly_reset_override_ms: moved }), NOW)[0].text, `90% used, resets ${atText(moved, NOW)}`);
});

test("an account is near its limit from 80 up to but not at 100, on either window, as it stands now", () => {
  assert.equal(isNear(limits({ weekly: used(80) }), NOW), true);
  assert.equal(isNear(limits({ weekly: used(79) }), NOW), false);
  assert.equal(isNear(limits({ weekly: used(100) }), NOW), false);
  assert.equal(isNear(limits({ five_hour: used(85) }), NOW), true);
  assert.equal(isNear(limits({ weekly: used(100), five_hour: used(85) }), NOW), true);
  assert.equal(isNear(limits({ weekly: used(90, NOW - 1) }), NOW), false);
  assert.equal(isNear(limits({}), NOW), false);
});
