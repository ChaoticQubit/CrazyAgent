import assert from "node:assert/strict";
import test from "node:test";
import { bundle } from "./browser.mjs";
import { CONTRAST_MIN, press, tabRunner } from "./settings-tab-steps.mjs";

const fixture = await bundle(new URL("./fixtures/settings-pools-layout.mjs", import.meta.url).pathname);
const onPools = tabRunner(fixture);
const rows = (page) => page.evaluate("Fixture.orderRows()");
const commandCount = async (page, name) => (await page.evaluate("Fixture.commands()")).filter((command) => command === name).length;
const ITEM = (at) => `.pool-items .pool-item:nth-child(${at})`;
const RANKING = "desktop_account_ranking";
const SAVE = "desktop_set_account_schedule";
const SCHEDULE_READ = "desktop_account_schedule";
const PIN = (name) => `[role="radiogroup"][aria-label="Pin for ${name}"] [role="radio"]`;
const SCHEDULE_OF = (name) => `button[aria-label="Schedule for ${name}"]`;
const ORDER = '[role="radiogroup"][aria-label="Account order"] [role="radio"]';
const APP_LABEL = "How every pool picks an account";
const APP = `[role="radiogroup"][aria-label="${APP_LABEL}"] [role="radio"]`;
const FULL_SENTENCE = "Too many saved account choices. Clear some you no longer need, then try again.";
const STALE_SENTENCE = "This changed since you opened it. Review and save again.";
const RANK_RULE = "Use a rank from 1 to 999, or leave it empty.";
const RESERVE_RULE = "Use a reserve from 0 to 99 percent.";
const PLAN_1 = { rank: 2, reserve_percent: 10, pin: "use_last" };
const NO_PLAN = { rank: null, reserve_percent: 0, pin: null };
const MANUAL_MAIN = { pools: [{ id: "pool-main", strategy: "manual_rank" }] };
const sent = (page) => page.evaluate("Fixture.savedChanges()");
const savedLine = (page) => page.evaluate("Fixture.wordsOf('.pool-saved')");
const checked = (page, label) => page.evaluate(`Fixture.checked(${JSON.stringify(label)})`);
const focusAt = (page) => page.evaluate("Fixture.focusPlace()");
const valueOf = (page, id) => page.evaluate(`document.getElementById(${JSON.stringify(id)}).value`);
const accountChange = (connection, plan, version = 4) => ({ change: { kind: "account", connection, plan }, version });
const rowNames = async (page) => (await rows(page)).map((row) => row.name);

/** Presses and lets go of one real key. Inputs: the page, the key and its key code. */
async function pressKey(page, key, code) {
  for (const type of ["keyDown", "keyUp"]) await page.send("Input.dispatchKeyEvent", { type, key, code: key, windowsVirtualKeyCode: code });
  await page.evaluate("Fixture.settle(80)");
}

/** Replaces a field's words with real input. Inputs: the page, the field's id and the words. */
async function typeInto(page, id, text) {
  await press(page, `#${id}`);
  await page.evaluate(`document.getElementById(${JSON.stringify(id)}).select()`);
  if (text === "") await pressKey(page, "Backspace", 8);
  else await page.send("Input.insertText", { text });
  await page.evaluate("Fixture.settle(20)");
}

/** Opens one account's Schedule panel and types into a field of it, then ends with a key.
 * Inputs: the page, the account's name, the field's id, the words and the key to end with. */
async function fillAndLeave(page, name, id, text, key) {
  await press(page, SCHEDULE_OF(name));
  await typeInto(page, id, text);
  if (key === "Enter") await pressKey(page, "Enter", 13);
  else await pressKey(page, "Tab", 9);
}

for (const [name, connection, option, plan, line] of [
  ["Account 1", "acct-1", "None", { ...PLAN_1, pin: null }, "ChatGPT · Rank 2, keeps 10% of its week"],
  ["Account 0", "acct-0", "Use last", { ...NO_PLAN, pin: "use_last" }, "ChatGPT · No rank, used to 100%, used last"],
  ["Account 1", "acct-1", "Never use", { ...PLAN_1, pin: "never_use" }, "ChatGPT · Never used"],
]) {
  test(`Pin ${option} on ${name} sends one change holding the rest of the plan and says so`, async () => {
    const seen = await onPools({}, async (page) => {
      await press(page, PIN(name), option);
      return { sent: await sent(page), saved: await savedLine(page), checked: await checked(page, `Pin for ${name}`), rows: await rows(page) };
    });
    assert.deepEqual(seen.sent, [accountChange(connection, plan)]);
    assert.equal(seen.saved, `Pin for ${name}: ${option}.`);
    assert.equal(seen.checked, option);
    assert.equal(seen.rows.find((row) => row.name === name).plan, line);
  });
}

test("real arrow keys move a pin and the focus stays on the option picked", async () => {
  const seen = await onPools({}, async (page) => {
    await page.evaluate(`document.querySelector('${PIN("Account 0")}[aria-checked="true"]').focus()`);
    await pressKey(page, "ArrowRight", 39);
    return { sent: await sent(page), focus: await focusAt(page) };
  });
  assert.deepEqual(seen.sent, [accountChange("acct-0", { ...NO_PLAN, pin: "use_last" })]);
  assert.deepEqual([seen.focus.group, seen.focus.words], ["Pin for Account 0", "Use last"]);
});

for (const [how, key] of [["Enter", "Enter"], ["leaving the field", "Tab"]]) {
  test(`Rank saved by ${how} sends one change holding the rest of the plan and says so`, async () => {
    const seen = await onPools({}, async (page) => {
      await fillAndLeave(page, "Account 1", "pool-rank-acct-1", "5", key);
      return { sent: await sent(page), saved: await savedLine(page) };
    });
    assert.deepEqual(seen.sent, [accountChange("acct-1", { ...PLAN_1, rank: 5 })]);
    assert.equal(seen.saved, "Rank for Account 1 saved.");
  });

  test(`Reserve saved by ${how} sends one change holding the rest of the plan and says so`, async () => {
    const seen = await onPools({}, async (page) => {
      await fillAndLeave(page, "Account 1", "pool-reserve-acct-1", "20", key);
      return { sent: await sent(page), saved: await savedLine(page) };
    });
    assert.deepEqual(seen.sent, [accountChange("acct-1", { ...PLAN_1, reserve_percent: 20 })]);
    assert.equal(seen.saved, "Reserve for Account 1 saved.");
  });
}

test("Enter and then leaving the field sends the rank once", async () => {
  const seen = await onPools({}, async (page) => {
    await fillAndLeave(page, "Account 1", "pool-rank-acct-1", "5", "Enter");
    await pressKey(page, "Tab", 9);
    return sent(page);
  });
  assert.equal(seen.length, 1, JSON.stringify(seen));
});

test("an empty Rank clears the rank and an empty Reserve means no reserve", async () => {
  const seen = await onPools({}, async (page) => {
    await fillAndLeave(page, "Account 1", "pool-rank-acct-1", "", "Enter");
    await typeInto(page, "pool-reserve-acct-1", "");
    await pressKey(page, "Enter", 13);
    return sent(page);
  });
  assert.deepEqual(seen, [accountChange("acct-1", { ...PLAN_1, rank: null }), accountChange("acct-1", { rank: null, reserve_percent: 0, pin: "use_last" }, 5)]);
});

for (const [label, id, typed, rule] of [
  ["Rank", "pool-rank-acct-1", "0", RANK_RULE],
  ["Rank", "pool-rank-acct-1", "1000", RANK_RULE],
  ["Rank", "pool-rank-acct-1", "abc", RANK_RULE],
  ["Reserve", "pool-reserve-acct-1", "100", RESERVE_RULE],
  ["Reserve", "pool-reserve-acct-1", "abc", RESERVE_RULE],
]) {
  test(`${label} ${typed} shows the rule, marks the field and sends nothing`, async () => {
    const seen = await onPools({}, async (page) => {
      await fillAndLeave(page, "Account 1", id, typed, "Enter");
      return { sent: await sent(page), rule: await page.evaluate(`Fixture.wordsOf('.pool-sched [role="alert"]')`), invalid: await page.evaluate(`Fixture.invalid('#${id}')`), value: await valueOf(page, id) };
    });
    assert.deepEqual(seen.sent, []);
    assert.equal(seen.rule, rule);
    assert.equal(seen.invalid, "true");
    assert.equal(seen.value, typed, "the owner's words stay to be fixed");
  });
}

test("a reserve typed with a percent sign saves", async () => {
  const seen = await onPools({}, async (page) => {
    await fillAndLeave(page, "Account 1", "pool-reserve-acct-1", "5%", "Enter");
    return sent(page);
  });
  assert.deepEqual(seen, [accountChange("acct-1", { ...PLAN_1, reserve_percent: 5 })]);
});

test("leaving Rank and Reserve without a change sends nothing", async () => {
  const seen = await onPools({}, async (page) => {
    await press(page, SCHEDULE_OF("Account 1"));
    await press(page, "#pool-rank-acct-1");
    await pressKey(page, "Tab", 9);
    await pressKey(page, "Tab", 9);
    return { sent: await sent(page), rank: await valueOf(page, "pool-rank-acct-1"), reserve: await valueOf(page, "pool-reserve-acct-1") };
  });
  assert.deepEqual(seen, { sent: [], rank: "2", reserve: "10" });
});

test("Schedule opens one panel at a time with Rank, Reserve %, the plan and where it applies", async () => {
  const seen = await onPools({}, async (page) => {
    const before = await page.evaluate(`document.querySelector('${SCHEDULE_OF("Account 1")}').getAttribute("aria-expanded")`);
    await press(page, SCHEDULE_OF("Account 1"));
    const opened = { expanded: await page.evaluate(`document.querySelector('${SCHEDULE_OF("Account 1")}').getAttribute("aria-expanded")`), words: await page.evaluate("Fixture.wordsOf('.pool-sched')"), rank: await valueOf(page, "pool-rank-acct-1"), reserve: await valueOf(page, "pool-reserve-acct-1") };
    await press(page, SCHEDULE_OF("Account 0"));
    const other = { panels: await page.evaluate("Fixture.count('.pool-sched')"), first: await page.evaluate(`document.querySelector('${SCHEDULE_OF("Account 1")}').getAttribute("aria-expanded")`), rank: await valueOf(page, "pool-rank-acct-0"), reserve: await valueOf(page, "pool-reserve-acct-0") };
    await press(page, SCHEDULE_OF("Account 0"));
    return { before, opened, other, closed: await page.evaluate("Fixture.count('.pool-sched')") };
  });
  assert.equal(seen.before, "false");
  assert.equal(seen.opened.expanded, "true");
  assert.deepEqual([seen.opened.rank, seen.opened.reserve], ["2", "10"]);
  assert.match(seen.opened.words, /Rank/);
  assert.match(seen.opened.words, /Reserve %/);
  assert.match(seen.opened.words, /Rank 2, keeps 10% of its week, used last/);
  assert.match(seen.opened.words, /Applies in every pool this account is in\./);
  assert.deepEqual(seen.other, { panels: 1, first: "false", rank: "", reserve: "0" });
  assert.equal(seen.closed, 0);
});

for (const [pool, poolId, at, option, strategy] of [
  ["Spare", "pool-spare", 2, "Default (Use it or lose it)", null],
  ["Main pool", "pool-main", 1, "Use it or lose it", "use_it_or_lose_it"],
  ["Main pool", "pool-main", 1, "Manual rank", "manual_rank"],
  ["Main pool", "pool-main", 1, "Balanced", "balanced"],
]) {
  test(`Account order ${option} on ${pool} sends one change and says so`, async () => {
    const seen = await onPools({}, async (page) => {
      await press(page, ITEM(at));
      await press(page, ORDER, option);
      return { sent: await sent(page), saved: await savedLine(page), checked: await checked(page, "Account order") };
    });
    assert.deepEqual(seen.sent, [{ change: { kind: "pool_strategy", pool: poolId, strategy }, version: 4 }]);
    assert.equal(seen.saved, `Account order: ${option}.`);
    assert.equal(seen.checked, option);
  });
}

for (const [option, strategy, start, hint] of [
  ["Use it or lose it", "use_it_or_lose_it", { strategy: "balanced" }, "The account whose weekly limit resets soonest goes first, then your rank."],
  ["Manual rank", "manual_rank", {}, "Accounts go in the order you rank them."],
  ["Balanced", "balanced", {}, "The account with the least weekly use goes first."],
]) {
  test(`How every pool picks an account: ${option} sends one change, says so and shows its hint`, async () => {
    const seen = await onPools({ start }, async (page) => {
      await press(page, APP, option);
      return { sent: await sent(page), saved: await savedLine(page), checked: await checked(page, APP_LABEL), hint: await page.evaluate("Fixture.wordsOf('.pool-app-hint')"), orders: await page.evaluate(`Fixture.optionsOf("Account order")`) };
    });
    assert.deepEqual(seen.sent, [{ change: { kind: "app_strategy", strategy }, version: 4 }]);
    assert.equal(seen.saved, `Every pool now defaults to ${option}.`);
    assert.equal(seen.checked, option);
    assert.equal(seen.hint, hint);
    assert.equal(seen.orders[0], `Default (${option})`);
  });
}

test("Rank moves a row under Manual rank", async () => {
  const seen = await onPools({ ranking: "live", start: MANUAL_MAIN }, async (page) => {
    const before = await rowNames(page);
    await fillAndLeave(page, "Account 0", "pool-rank-acct-0", "1", "Enter");
    return { before, after: await rowNames(page), order: await page.evaluate("Fixture.wordsOf('.pool-order p')") };
  });
  assert.deepEqual(seen.before, ["Account 2", "Account 0", "Account 1"]);
  assert.deepEqual(seen.after, ["Account 0", "Account 2", "Account 1"]);
  assert.match(seen.order, /Set a rank under Schedule to move a row\./);
});

for (const [order, start, words] of [
  ["Use it or lose it", {}, /resets soonest goes first, then your rank\./],
  ["Balanced", { pools: [{ id: "pool-main", strategy: "balanced" }] }, /Rank only breaks ties\./],
]) {
  test(`under ${order} a rank saves, the field stays, the row does not move and the order line says what rank does`, async () => {
    const seen = await onPools({ ranking: "live", start }, async (page) => {
      await fillAndLeave(page, "Account 0", "pool-rank-acct-0", "1", "Enter");
      return { sent: await sent(page), after: await rowNames(page), field: await valueOf(page, "pool-rank-acct-0"), order: await page.evaluate("Fixture.wordsOf('.pool-order p')") };
    });
    assert.equal(seen.sent.length, 1);
    assert.deepEqual(seen.after, ["Account 2", "Account 0", "Account 1"]);
    assert.equal(seen.field, "1");
    assert.match(seen.order, words);
  });
}

for (const [what, act, read, back] of [
  ["pin", (page) => press(page, PIN("Account 0"), "Never use"), (page) => checked(page, "Pin for Account 0"), "None"],
  ["rank", (page) => fillAndLeave(page, "Account 1", "pool-rank-acct-1", "5", "Enter"), (page) => valueOf(page, "pool-rank-acct-1"), "2"],
  ["reserve", (page) => fillAndLeave(page, "Account 1", "pool-reserve-acct-1", "20", "Enter"), (page) => valueOf(page, "pool-reserve-acct-1"), "10"],
  ["pool order", (page) => press(page, ORDER, "Balanced"), (page) => checked(page, "Account order"), "Default (Use it or lose it)"],
  ["app order", (page) => press(page, APP, "Balanced"), (page) => checked(page, APP_LABEL), "Use it or lose it"],
]) {
  test(`a refused ${what} goes back and shows the engine's sentence`, async () => {
    const seen = await onPools({ refuse: "account_schedule_full" }, async (page) => {
      await act(page);
      return { sent: await sent(page), shown: await read(page), alert: await page.evaluate("Fixture.alert()"), saved: await savedLine(page) };
    });
    assert.equal(seen.sent.length, 1);
    assert.equal(seen.shown, back);
    assert.equal(seen.alert.words, FULL_SENTENCE);
    assert.equal(seen.saved, "");
  });
}

test("a refusal stays in the window with the page scrolled down to the last row", async () => {
  const seen = await onPools({ pools: "full", ranking: "full", refuse: "account_schedule_full" }, async (page) => {
    await press(page, PIN("Account 14"), "Never use");
    return { inView: await page.evaluate("Fixture.alertInView()"), scrolled: await page.evaluate("document.getElementById('main').scrollTop") };
  });
  assert.ok(seen.scrolled > 0, "the last row needs a scroll");
  assert.equal(seen.inView, true);
});

test("reaching the last row of a long pool scrolls the page pane and never the window itself", async () => {
  const seen = await onPools({ pools: "full", ranking: "full" }, async (page) => {
    await press(page, PIN("Account 14"), "Never use");
    return page.evaluate(`({ spare: document.documentElement.scrollHeight - document.documentElement.clientHeight, moved: document.documentElement.scrollTop, pane: document.getElementById("main").scrollTop })`);
  });
  assert.ok(seen.pane > 0, "the last row needs a scroll");
  assert.deepEqual({ spare: seen.spare, moved: seen.moved }, { spare: 0, moved: 0 });
});

test("a change made after a refusal takes the refusal away", async () => {
  const seen = await onPools({ refuse: "account_schedule_full", refuseAt: 1 }, async (page) => {
    await press(page, PIN("Account 0"), "Never use");
    const refused = await page.evaluate("Fixture.alert()");
    await press(page, PIN("Account 0"), "Use last");
    return { refused, after: await page.evaluate("Fixture.alert()") };
  });
  assert.equal(seen.refused.words, FULL_SENTENCE);
  assert.equal(seen.after.words, "");
});

test("past the bound of waiting changes the next is refused and says so, and once they drain a change applies", async () => {
  const seen = await onPools({}, async (page) => {
    await page.evaluate(`Fixture.gate("${SAVE}")`);
    await page.evaluate(`document.querySelector('${PIN("Account 0")}[aria-checked="true"]').focus()`);
    for (let at = 0; at < 66; at += 1) {
      for (const type of ["keyDown", "keyUp"]) await page.send("Input.dispatchKeyEvent", { type, key: "ArrowRight", code: "ArrowRight", windowsVirtualKeyCode: 39 });
    }
    await page.evaluate("Fixture.settle(120)");
    const refused = await page.evaluate("Fixture.alert()");
    await page.evaluate("Fixture.openGate()");
    await page.evaluate("Fixture.settle(400)");
    const drained = (await sent(page)).length;
    await press(page, PIN("Account 2"), "Never use");
    return { refused, drained, last: (await sent(page)).at(-1), after: await page.evaluate("Fixture.alert()") };
  });
  assert.equal(seen.refused.words, "Too many changes at once. Wait a moment and try again.");
  assert.equal(seen.drained, 64);
  assert.equal(seen.last.change.connection, "acct-2");
  assert.equal(seen.after.words, "");
});

test("a stale_revision reads the schedule again and says so", async () => {
  const seen = await onPools({ refuse: "stale_revision" }, async (page) => {
    const before = await commandCount(page, SCHEDULE_READ);
    await press(page, PIN("Account 0"), "Never use");
    return { before, after: await commandCount(page, SCHEDULE_READ), alert: await page.evaluate("Fixture.alert()"), shown: await checked(page, "Pin for Account 0") };
  });
  assert.equal(seen.after, seen.before + 2, "one read before the save and one after its refusal");
  assert.equal(seen.alert.words, STALE_SENTENCE);
  assert.equal(seen.shown, "None");
});

test("a burst of changes reads the order once, after the last one", async () => {
  const seen = await onPools({ ranking: "live" }, async (page) => {
    const before = await commandCount(page, RANKING);
    await page.evaluate(`Fixture.gate("${SAVE}")`);
    await press(page, PIN("Account 0"), "Use last");
    await press(page, PIN("Account 2"), "Never use");
    await page.evaluate("Fixture.openGate()");
    return { before, after: await commandCount(page, RANKING) };
  });
  assert.equal(seen.after, seen.before + 1);
});

test("two pins made before the first is answered apply in order, each with the version the last one left, and both stay", async () => {
  const seen = await onPools({ ranking: "live" }, async (page) => {
    await page.evaluate(`Fixture.gate("${SAVE}")`);
    await press(page, PIN("Account 0"), "Use last");
    await press(page, PIN("Account 2"), "Never use");
    const waiting = { first: await checked(page, "Pin for Account 0"), second: await checked(page, "Pin for Account 2"), saves: (await sent(page)).length };
    await page.evaluate("Fixture.openGate()");
    return { waiting, sent: await sent(page), first: await checked(page, "Pin for Account 0"), second: await checked(page, "Pin for Account 2"), alert: await page.evaluate("Fixture.alert()") };
  });
  assert.deepEqual(seen.waiting, { first: "Use last", second: "Never use", saves: 0 }, "picks show while their saves wait");
  assert.deepEqual(seen.sent, [accountChange("acct-0", { ...NO_PLAN, pin: "use_last" }, 4), accountChange("acct-2", { ...NO_PLAN, pin: "never_use" }, 5)]);
  assert.deepEqual([seen.first, seen.second], ["Use last", "Never use"]);
  assert.equal(seen.alert.words, "");
});

test("a rank and a pin on one account made together both reach the plan", async () => {
  const seen = await onPools({}, async (page) => {
    await page.evaluate(`Fixture.gate("${SAVE}")`);
    await fillAndLeave(page, "Account 0", "pool-rank-acct-0", "7", "Enter");
    await press(page, PIN("Account 0"), "Never use");
    await page.evaluate("Fixture.openGate()");
    return sent(page);
  });
  assert.deepEqual(seen, [accountChange("acct-0", { ...NO_PLAN, rank: 7 }, 4), accountChange("acct-0", { rank: 7, reserve_percent: 0, pin: "never_use" }, 5)]);
});

test("a save reads the schedule and the order again, and changes made together read them once", async () => {
  const seen = await onPools({}, async (page) => {
    const start = { order: await commandCount(page, RANKING), schedule: await commandCount(page, SCHEDULE_READ) };
    await press(page, PIN("Account 0"), "Use last");
    const one = { order: await commandCount(page, RANKING), schedule: await commandCount(page, SCHEDULE_READ) };
    await page.evaluate(`Fixture.gate("${SAVE}")`);
    await press(page, PIN("Account 0"), "Never use");
    await press(page, PIN("Account 2"), "Use last");
    await page.evaluate("Fixture.openGate()");
    return { start, one, two: { order: await commandCount(page, RANKING), schedule: await commandCount(page, SCHEDULE_READ) } };
  });
  assert.equal(seen.one.order, seen.start.order + 1);
  assert.equal(seen.one.schedule, seen.start.schedule + 2, "one read before the save and one after");
  assert.equal(seen.two.order, seen.one.order + 1);
  assert.equal(seen.two.schedule, seen.one.schedule + 3, "one read before each save and one after both");
});

test("after a pin moves its row the focus is still on that account's pin", async () => {
  const seen = await onPools({ ranking: "live" }, async (page) => {
    const before = await rowNames(page);
    await press(page, PIN("Account 2"), "Use last");
    return { before, after: await rowNames(page), focus: await focusAt(page) };
  });
  assert.deepEqual(seen.before, ["Account 2", "Account 0", "Account 1"]);
  assert.deepEqual(seen.after, ["Account 0", "Account 2", "Account 1"]);
  assert.deepEqual([seen.focus.group, seen.focus.words], ["Pin for Account 2", "Use last"]);
});

test("after Never use takes a row into the held list the focus is still on that account's pin", async () => {
  const seen = await onPools({ ranking: "live" }, async (page) => {
    await press(page, PIN("Account 0"), "Never use");
    return { held: (await rows(page)).filter((row) => row.held).map((row) => row.name), focus: await focusAt(page) };
  });
  assert.deepEqual(seen.held, ["Account 0"]);
  assert.deepEqual([seen.focus.group, seen.focus.words], ["Pin for Account 0", "Never use"]);
});

test("after Enter moves a row the focus is still in its Rank field and the panel is still open", async () => {
  const seen = await onPools({ ranking: "live", start: { pools: [{ id: "pool-main", strategy: "manual_rank" }], accounts: [{ connection: "acct-2", plan: { ...NO_PLAN, rank: 1 } }, { connection: "acct-0", plan: { ...NO_PLAN, rank: 2 } }] } }, async (page) => {
    const before = await rowNames(page);
    await fillAndLeave(page, "Account 2", "pool-rank-acct-2", "9", "Enter");
    return { before, after: await rowNames(page), focus: await focusAt(page), panels: await page.evaluate("Fixture.count('.pool-sched')") };
  });
  assert.deepEqual(seen.before, ["Account 2", "Account 0", "Account 1"]);
  assert.deepEqual(seen.after, ["Account 0", "Account 2", "Account 1"]);
  assert.equal(seen.focus.id, "pool-rank-acct-2");
  assert.equal(seen.panels, 1);
});

test("a read that moves a row does not pull the focus back from the pool list", async () => {
  const seen = await onPools({ ranking: "live" }, async (page) => {
    await page.evaluate(`Fixture.gate("${RANKING}")`);
    await press(page, PIN("Account 2"), "Use last");
    await page.evaluate(`document.querySelector("${ITEM(2)}").focus()`);
    await page.evaluate("Fixture.openGate()");
    return { after: await rowNames(page), focus: await focusAt(page) };
  });
  assert.deepEqual(seen.after, ["Account 0", "Account 2", "Account 1"]);
  assert.match(seen.focus.words, /^Spare/);
});

test("the saved line is one polite status, empty until a change is applied and empty again after six seconds", async () => {
  const seen = await onPools({}, async (page) => {
    const status = await page.evaluate("Fixture.count('.pool-saved[role=\"status\"]')");
    const before = await savedLine(page);
    await press(page, PIN("Account 0"), "Use last");
    const said = await savedLine(page);
    await page.evaluate("Fixture.fireSavedLine()");
    return { status, before, said, after: await savedLine(page), still: await page.evaluate("Fixture.count('.pool-saved[role=\"status\"]')") };
  });
  assert.deepEqual(seen, { status: 1, before: "", said: "Pin for Account 0: Use last.", after: "", still: 1 });
});

test("a schedule that cannot be read leaves no controls and offers Try again, which reads it again", async () => {
  const seen = await onPools({ schedule: "error" }, async (page) => {
    const before = await commandCount(page, SCHEDULE_READ);
    const pins = await page.evaluate("Fixture.count('[aria-label^=\"Pin for\"]')");
    await press(page, ".pool-left button", "Try again");
    return { pins, left: await page.evaluate("Fixture.wordsOf('.pool-left')"), before, after: await commandCount(page, SCHEDULE_READ) };
  });
  assert.equal(seen.pins, 0);
  assert.match(seen.left, /Couldn't load account choice\./);
  assert.equal(seen.after, seen.before + 1);
});

test("a build with no account choice says so in the left column and shows no controls", async () => {
  const seen = await onPools({ schedule: "missing" }, async (page) => ({ left: await page.evaluate("Fixture.wordsOf('.pool-left')"), pins: await page.evaluate("Fixture.count('[aria-label^=\"Pin for\"]')") }));
  assert.match(seen.left, /Account choice isn't ready/);
  assert.equal(seen.pins, 0);
});

for (const width of [820, 1180, 1200, 1440]) {
  test(`the row controls and an open panel stay inside their rows at ${width} px`, async () => {
    const seen = await onPools({}, async (page) => {
      await press(page, SCHEDULE_OF("Account 1"));
      return { fits: await page.evaluate("Fixture.controlsFit()"), panes: await page.evaluate("Fixture.panes()") };
    }, width);
    assert.equal(seen.fits, true);
    assert.equal(seen.panes.sideways, false, JSON.stringify(seen.panes));
  });
}

for (const theme of ["light", "dark"]) {
  test(`the saved line, the panel's words and the app order hint pass contrast in the ${theme} theme`, async () => {
    const seen = await onPools({}, async (page) => {
      await press(page, SCHEDULE_OF("Account 1"));
      await press(page, PIN("Account 0"), "Use last");
      const read = {};
      for (const selector of [".pool-saved", ".pool-sched-text", ".pool-sched-text b", ".pool-app-hint"]) read[selector] = await page.evaluate(`Fixture.contrast(${JSON.stringify(selector)})`);
      return read;
    }, 1440, theme);
    for (const [name, ratio] of Object.entries(seen)) assert.ok(ratio >= CONTRAST_MIN, `${name}: ${ratio}`);
  });
}

for (const [what, selector, option] of [["pin", PIN("Account 0"), "None"], ["pool order", ORDER, "Default (Use it or lose it)"], ["app order", APP, "Use it or lose it"]]) {
  test(`pressing the ${what} already picked sends nothing`, async () => {
    const seen = await onPools({}, async (page) => {
      await press(page, selector, option);
      return { sent: await sent(page), saved: await savedLine(page) };
    });
    assert.deepEqual(seen, { sent: [], saved: "" });
  });
}
