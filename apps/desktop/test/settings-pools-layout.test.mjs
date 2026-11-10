import assert from "node:assert/strict";
import test from "node:test";
import { bundle } from "./browser.mjs";
import { doublePress, CONTRAST_MIN, press, tabRunner, tabWalk } from "./settings-tab-steps.mjs";

const fixture = await bundle(new URL("./fixtures/settings-pools-layout.mjs", import.meta.url).pathname);
const onPools = tabRunner(fixture);
const EDIT_MAIN = 'button[aria-label="Edit pool Main pool"]';

const read = (page) => page.evaluate("Fixture.sections()");
const say = (page) => page.evaluate("Fixture.text()");
const pane = (page) => page.evaluate("Fixture.wordsOf('.pool-detail')");
const items = (page) => page.evaluate("Fixture.poolItems()");
const rows = (page) => page.evaluate("Fixture.orderRows()");
const commandCount = async (page, name) => (await page.evaluate("Fixture.commands()")).filter((command) => command === name).length;
const ITEM = (at) => `.pool-items .pool-item:nth-child(${at})`;
const RANKING = "desktop_account_ranking";
const USAGE = "desktop_usage_report";
const POOLS_READ = "desktop_pool_summaries";
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

test("the screen shows the title and purpose, Your pools with New pool on the left, and the first pool selected on the right", async () => {
  const seen = await onPools({}, async (page) => ({ top: await page.evaluate("Fixture.top()"), sections: await read(page), items: await items(page), cap: await page.evaluate("Fixture.wordsOf('.pool-cap')") }));
  assert.deepEqual(seen.top?.heading, "Pools", JSON.stringify(seen.top));
  assert.match(seen.top.purpose, /A pool is a group of accounts for a project\. When one account runs out, agents move to the next one in the pool\./);
  assert.deepEqual(seen.top.actions, []);
  assert.deepEqual(seen.sections.map((section) => [section.heading, section.actions]), [["Your pools", ["New pool"]], ["Main pool", ["Get ready for agents", "Edit pool", "Delete pool"]]]);
  assert.equal(seen.sections[1].about, "Used by 2 projects. 3 ready. Shown on each project's pool picker.");
  assert.deepEqual(seen.items.map((item) => [item.name, item.count, item.about, item.current]), [["Main pool", "3 of 15", "3 ready. Used by 2 projects.", "true"], ["Spare", "1 of 15", "1 ready. Not used.", null]]);
  assert.equal(seen.cap, "3 of 15");
});

test("pressing a pool selects it and shows its own accounts, actions and order, and the focus stays on it", async () => {
  const seen = await onPools({}, async (page) => {
    await press(page, ITEM(2));
    return { items: await items(page), sections: await read(page), rows: await rows(page), order: await page.evaluate("Fixture.wordsOf('.pool-order p')"), head: await page.evaluate("Fixture.wordsOf('.pool-order-head')"), focused: await page.evaluate("Fixture.focused()") };
  });
  assert.deepEqual(seen.items.map((item) => item.current), [null, "true"]);
  assert.equal(seen.sections[1].heading, "Spare");
  assert.deepEqual(seen.sections[1].actions, ["Edit pool", "Delete pool"], "only a pool with keyed members can be set up for agents");
  assert.deepEqual(seen.rows.map((row) => row.name), ["Account 3"]);
  assert.equal(seen.head, "Account order");
  assert.match(seen.order, /^Order comes from Balanced, this pool: /);
  assert.match(seen.order, /1 project using this pool sets its own order\.$/);
  assert.match(seen.focused.name, /^Spare/);
});

test("a 60-letter pool name wraps in the list and in the pane and nothing moves sideways", async () => {
  const seen = await onPools({ pools: "long" }, async (page) => ({ items: await items(page), sections: await read(page), panes: await page.evaluate("Fixture.panes()") }), 1440);
  assert.equal(seen.items[0].name.length, 60);
  assert.equal(seen.items[0].spills, false, JSON.stringify(seen.items[0]));
  assert.deepEqual(seen.sections[1].actions, ["Edit pool", "Delete pool"]);
  assert.equal(seen.panes.sideways, false, JSON.stringify(seen.panes));
});

test("real Tab keys walk New pool, each pool in the list, then the selected pool's actions", async () => {
  const walked = await onPools({}, (page) => tabWalk(page, 6));
  const from = walked.indexOf("New pool");
  assert.notEqual(from, -1, JSON.stringify(walked));
  const names = walked.slice(from, from + 6);
  assert.match(names[1], /^Main pool/);
  assert.match(names[2], /^Spare/);
  assert.deepEqual(names.slice(3), ["Get ready for agents: Main pool", "Edit pool Main pool", "Delete pool Main pool"]);
});

test("the rows stand exactly as the ranking gives them, each with its place, name, state, plan, why and usage", async () => {
  const seen = await onPools({}, rows);
  assert.deepEqual(seen.map((row) => [row.number, row.spoken, row.name, row.held]), [["1", "Position 1", "Account 2", false], ["2", "Position 2", "Account 0", false], ["3", "Position 3", "Account 1", false]]);
  assert.deepEqual(seen.map((row) => row.badges), [["Signed in"], ["Signed in"], ["Signed in"]]);
  assert.deepEqual(seen.map((row) => row.plan), ["ChatGPT \u00b7 No rank, used to 100%", "ChatGPT \u00b7 No rank, used to 100%", "ChatGPT \u00b7 Rank 2, keeps 10% of its week, used last"]);
  assert.deepEqual(seen.map((row) => row.why), ["You ranked it 1.", "Least weekly use of the accounts ready now.", "More weekly use than the one above."]);
  assert.deepEqual(seen[0].limits, [["Weekly62% used", "ok"], ["5-hour18% used", "ok"]]);
  assert.deepEqual(seen[1].limits, [["Weekly30% used", "ok"]]);
  assert.deepEqual([seen[2].limits, seen[2].none], [[], "No usage read yet"]);
});

test("held accounts come after the held line with no number, in the ranking's order", async () => {
  const seen = await onPools({ ranking: "held" }, async (page) => ({ rows: await rows(page), head: await page.evaluate("Fixture.heldHead()") }));
  assert.deepEqual(seen.rows.map((row) => [row.number, row.spoken, row.name, row.held]), [["1", "Position 1", "Account 2", false], ["-", "Held", "Account 1", true], ["-", "Held", "Account 0", true]]);
  assert.deepEqual(seen.head, { words: "Held now, in pool order. Each one rejoins the order when its reason ends.", between: true });
  assert.equal(seen.rows[2].why, "Never use. You pinned it.");
});

test("a not signed in row reads why and links to its own sign-in, and no other row has the link", async () => {
  const seen = await onPools({ ranking: "held" }, rows);
  assert.equal(seen[1].why, "Not signed in. Agents skip it until you sign in again. Sign in");
  assert.deepEqual(seen[1].link, { href: "#/settings/accounts?action=sign_in&account=acct-1", label: "Sign in Account 1" });
  assert.deepEqual([seen[0].link, seen[2].link], [null, null]);
});

test("with no ready account the pane says so and still lists the held ones", async () => {
  const seen = await onPools({ ranking: "none" }, async (page) => ({ says: await pane(page), rows: await rows(page) }));
  assert.ok(seen.says.includes("No account can take work now."), seen.says);
  assert.deepEqual(seen.rows.map((row) => row.spoken), ["Held", "Held", "Held"]);
});

test("Near its limit shows at 80 and not at 79, and a used-up window reads full and not near", async () => {
  const seen = await onPools({ limits: "near" }, rows);
  assert.deepEqual(seen.map((row) => [row.name, row.badges.includes("Near its limit"), row.limits[0][1]]), [["Account 2", true, "near"], ["Account 0", false, "ok"], ["Account 1", false, "full"]]);
});

for (const [scene, words, absent] of [
  [{ ranking: "missing" }, ["Account order isn't ready yet"], "No account can take work now."],
  [{ ranking: "error" }, ["Couldn't load the account order.", "Try again"], "No account can take work now."],
  [{ ranking: "bare" }, ["No accounts to rank yet."], "No account can take work now."],
  [{ limits: "error" }, ["Couldn't read usage limits."], "No usage read yet"],
  [{ limits: "pending" }, ["Account 2"], "No usage read yet"],
  [{ limits: "missing" }, ["Account 2"], "No usage read yet"],
]) {
  test(`the pane keeps its state for ${JSON.stringify(scene)}`, async () => {
    const seen = await onPools(scene, async (page) => ({ says: await pane(page), items: await items(page) }));
    for (const part of words) assert.ok(seen.says.includes(part), `${part} in ${seen.says}`);
    assert.ok(!seen.says.includes(absent), seen.says);
    assert.equal(seen.items.length, 2, "the list stays when one read of the pane fails");
  });
}

test("a ranking still loading draws skeletons in the pane and leaves the list in place", async () => {
  const seen = await onPools({ ranking: "pending" }, async (page) => ({ skeletons: await page.evaluate("Fixture.count('.pool-detail .skeleton')"), items: await items(page) }));
  assert.ok(seen.skeletons >= 4, String(seen.skeletons));
  assert.equal(seen.items.length, 2);
});

test("Try again on the order reads the ranking a second time", async () => {
  const reads = await onPools({ ranking: "error" }, async (page) => {
    await press(page, ".pool-detail button", "Try again");
    return commandCount(page, RANKING);
  });
  assert.ok(reads >= 2, String(reads));
});

test("with the schedule not read, a row keeps its harness and drops only the plan", async () => {
  const seen = await onPools({ schedule: "error" }, async (page) => ({ rows: await rows(page), order: await page.evaluate("Fixture.wordsOf('.pool-order')") }));
  assert.equal(seen.rows[0].plan, "ChatGPT");
  assert.equal(seen.order, "");
});

test("a pool with no accounts says how to add one", async () => {
  const says = await onPools({ pools: "hollow", ranking: "bare" }, pane);
  assert.ok(says.includes("No accounts yet. Use Edit pool to put one in this pool."), says);
  assert.ok(!says.includes("No accounts to rank yet."), says);
});

test("the two panes stand side by side above 1180 px and stack at 1180 px, and rows keep room for their words", async () => {
  const wide = await onPools({}, async (page) => ({ panes: await page.evaluate("Fixture.panes()"), rows: await rows(page) }), 1440);
  assert.ok(wide.panes.list.right <= wide.panes.pool.left, JSON.stringify(wide.panes));
  assert.equal(wide.panes.list.top, wide.panes.pool.top);
  assert.equal(wide.panes.list.right - wide.panes.list.left, 232);
  assert.ok(wide.rows[0].whoWidth >= 200, String(wide.rows[0].whoWidth));
  const justAbove = await onPools({}, async (page) => ({ panes: await page.evaluate("Fixture.panes()"), rows: await rows(page) }), 1200);
  assert.ok(justAbove.panes.list.right <= justAbove.panes.pool.left, JSON.stringify(justAbove.panes));
  assert.ok(justAbove.rows[0].whoWidth >= 200, `a pane this narrow still leaves the name room: ${justAbove.rows[0].whoWidth}`);
  assert.equal(justAbove.panes.sideways, false);
  for (const width of [1180, 900, 700]) {
    const panes = await onPools({}, (page) => page.evaluate("Fixture.panes()"), width);
    assert.ok(panes.pool.top > panes.list.top, `${width}: ${JSON.stringify(panes)}`);
    assert.equal(panes.pool.left, panes.list.left, String(width));
    assert.equal(panes.sideways, false, String(width));
  }
});

test("the page reads the ranking and the limits again once a minute and leaves the pools read alone", async () => {
  const seen = await onPools({}, async (page) => {
    const delays = await page.evaluate("Fixture.timerDelays()");
    const before = [await commandCount(page, RANKING), await commandCount(page, USAGE), await commandCount(page, POOLS_READ)];
    await page.evaluate("Fixture.fireTimer(60000)");
    return { delays, before, after: [await commandCount(page, RANKING), await commandCount(page, USAGE), await commandCount(page, POOLS_READ)] };
  });
  assert.equal(seen.delays.filter((delay) => delay === 60000).length, 1, JSON.stringify(seen.delays));
  assert.deepEqual(seen.after, [seen.before[0] + 1, seen.before[1] + 1, seen.before[2]]);
});

test("a deleted pool reads the pools and the order again", async () => {
  const seen = await onPools({ edits: "ok" }, async (page) => {
    await press(page, ITEM(2));
    const before = [await commandCount(page, POOLS_READ), await commandCount(page, RANKING)];
    await press(page, 'button[aria-label="Delete pool Spare"]');
    await press(page, "dialog[open] button", "Delete pool");
    return { before, after: [await commandCount(page, POOLS_READ), await commandCount(page, RANKING)] };
  });
  assert.deepEqual(seen.after, [seen.before[0] + 1, seen.before[1] + 1]);
});

test("the page asks usage for a short range, since it reads only the limits", async () => {
  const since = await onPools({}, (page) => page.evaluate("Fixture.lastUsageSince()"));
  assert.equal(typeof since, "number", String(since));
  assert.ok(since <= Date.now(), "a range start past the engine's clock is refused");
  assert.ok(since >= Date.now() - 2 * 60 * 60 * 1000, new Date(since).toISOString());
});

test("a change saved in Account choice reads the order on the right again", async () => {
  const seen = await onPools({}, async (page) => {
    const before = await commandCount(page, RANKING);
    await press(page, 'form.row button[type="submit"]');
    return { before, after: await commandCount(page, RANKING), saved: await commandCount(page, "desktop_set_account_schedule") };
  });
  assert.equal(seen.saved, 1);
  assert.equal(seen.after, seen.before + 1);
});

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

for (const [state, expected, absent] of [
  ["empty", ["No pools yet", "A pool groups up to 15 accounts for your projects to use.", "New pool"], "Your pools"],
  ["error", ["Couldn't load pools.", "Try again"], "New pool"],
  ["missing", ["Pools isn't ready yet"], "New pool"],
]) {
  test(`the list keeps its ${state} state under the title`, async () => {
    const seen = await onPools({ pools: state }, async (page) => ({ top: await page.evaluate("Fixture.top()"), says: await say(page) }));
    assert.equal(seen.top?.heading, "Pools", JSON.stringify(seen));
    for (const part of expected) assert.ok(seen.says.includes(part), `${part} in ${seen.says}`);
    assert.deepEqual(seen.top.actions, [], "New pool stands in the empty state only when there is nothing to list");
    if (absent !== "New pool") assert.ok(!seen.says.includes(absent), seen.says);
  });
}

test("Edit pool opens the editor with Details, then All accounts beside In this pool, and Back to pools returns", async () => {
  const seen = await onPools({}, async (page) => {
    await press(page, EDIT_MAIN);
    const editor = { top: await page.evaluate("Fixture.top()"), sections: await read(page) };
    await press(page, ".settings-top button", "Back to pools");
    return { editor, back: await read(page) };
  });
  assert.equal(seen.editor.top?.heading, "Edit Main pool", JSON.stringify(seen.editor.top));
  assert.match(seen.editor.top.purpose, /Pick the accounts this pool can use\. A pool holds up to 15 accounts\./);
  assert.deepEqual(seen.editor.top.actions, ["Back to pools"]);
  assert.deepEqual(seen.editor.sections.map((section) => section.heading), ["Details", "All accounts", "In this pool"]);
  const [, all, pool] = seen.editor.sections;
  assert.equal(all.top, pool.top, "the two lists start level");
  assert.ok(all.right <= pool.left, JSON.stringify([all, pool]));
  assert.deepEqual(seen.back.map((section) => section.heading).slice(0, 1), ["Your pools"]);
});

test("under 980 px the two lists stack", async () => {
  const sections = await onPools({}, async (page) => { await press(page, EDIT_MAIN); return read(page); }, 900);
  const [, all, pool] = sections;
  assert.ok(pool.top > all.top, JSON.stringify([all.top, pool.top]));
  assert.equal(pool.left, all.left);
});

test("Pool name is marked changed against the saved name, and In this pool against the saved members", async () => {
  const seen = await onPools({}, async (page) => {
    await press(page, EDIT_MAIN);
    const first = await read(page);
    await press(page, 'label[for="pool-name"]');
    const focused = await page.evaluate("Fixture.focused()");
    await page.send("Input.insertText", { text: "x" });
    await page.evaluate("Fixture.settle(40)");
    const named = await read(page);
    await press(page, '.member button[aria-label="Remove Account 1"]');
    return { first, focused, named, removed: await read(page) };
  });
  assert.equal(seen.focused.id, "pool-name", "a real click on the label focuses the field");
  assert.deepEqual([seen.first[0].rows[0].label, seen.first[0].rows[0].mark, seen.first[2].mark], ["Pool name", null, null]);
  assert.equal(seen.named[0].rows[0].mark, "changed");
  assert.equal(seen.removed[2].mark, "changed");
});

test("the editor keeps its name error, its full-pool notice and its save failure", async () => {
  const named = await onPools({}, async (page) => {
    await press(page, ".set-head button", "New pool");
    await press(page, 'button[type="submit"]');
    const empty = { says: await say(page), invalid: await page.evaluate("Fixture.invalid('#pool-name')") };
    await page.evaluate("document.getElementById('pool-name').focus()");
    await page.send("Input.insertText", { text: "Fresh" });
    await page.evaluate("Fixture.settle(40)");
    await press(page, 'button[type="submit"]');
    return { empty, failed: await say(page) };
  });
  assert.ok(named.empty.says.includes("Enter a pool name."), named.empty.says);
  assert.equal(named.empty.invalid, "true");
  assert.ok(named.failed.includes("Couldn't save this pool."), named.failed);
  const full = await onPools({ pools: "full" }, async (page) => { await press(page, EDIT_MAIN); return say(page); });
  assert.ok(full.includes("A pool can hold up to 15 accounts. Remove one to add another."), full);
  assert.ok(full.includes("15 of 15"), full);
});

for (const [scene, expected] of [
  [{ accounts: "empty" }, ["No accounts yet", "Add an account first, then put it in a pool.", "Add account"]],
  [{ accounts: "error" }, ["Couldn't load accounts."]],
  [{ accounts: "missing" }, ["Accounts isn't ready yet"]],
]) {
  test(`the account list keeps its ${scene.accounts} state`, async () => {
    const says = await onPools(scene, async (page) => { await press(page, ".set-head button", "New pool"); return say(page); });
    for (const part of expected) assert.ok(says.includes(part), `${part} in ${says}`);
  });
}

test("the account list keeps Provider, Selected only, search and their empty lines", async () => {
  const seen = await onPools({}, async (page) => {
    await press(page, ".set-head button", "New pool");
    const fresh = await say(page);
    const provider = await page.evaluate("Fixture.count('#pool-provider')");
    await press(page, "label.checkbox", "Selected only");
    const selected = await say(page);
    await press(page, "label.checkbox", "Selected only");
    await page.evaluate("document.querySelector('input[type=search]').focus()");
    await page.send("Input.insertText", { text: "nothing like this" });
    await page.evaluate("Fixture.settle(40)");
    return { fresh, provider, selected, searched: await say(page) };
  });
  assert.equal(seen.provider, 1);
  assert.ok(seen.fresh.includes("No accounts yet. Check accounts on the left to add them."), seen.fresh);
  for (const part of ["Selected only", "Save pool", "Cancel", "0 of 15"]) assert.ok(seen.fresh.includes(part), part);
  assert.ok(seen.selected.includes("No accounts selected yet."), seen.selected);
  assert.ok(seen.searched.includes("No accounts match."), seen.searched);
});

test("an account line shows name, provider and status with its checkbox on the right, and a tick adds a member", async () => {
  const seen = await onPools({}, async (page) => {
    await press(page, ".set-head button", "New pool");
    const pick = await page.evaluate("Fixture.pick()");
    await press(page, ".pick");
    return { pick, says: await say(page), members: await page.evaluate("Fixture.count('.member')"), sections: await read(page) };
  });
  assert.ok(seen.pick.boxLeft > seen.pick.nameRight, JSON.stringify(seen.pick));
  for (const part of ["Account 0", "ChatGPT", "Signed in"]) assert.ok(seen.pick.words.includes(part), `${part} in ${seen.pick.words}`);
  assert.equal(seen.members, 1);
  assert.ok(seen.says.includes("1 of 15"), seen.says);
  assert.equal(seen.sections[2].mark, "changed");
});

test("a full pool refuses another account on its own row and in the notice", async () => {
  const seen = await onPools({ pools: "full" }, async (page) => {
    await press(page, EDIT_MAIN);
    await press(page, ".pick:has(input:not(:checked))");
    return { says: await say(page), blocked: await page.evaluate("Fixture.count('.pick.blocked')"), line: await page.evaluate("Fixture.wordsOf('.pick.blocked')"), members: await page.evaluate("Fixture.count('.member')"), focused: await page.evaluate("Fixture.focused()") };
  });
  assert.equal(seen.blocked, 1);
  assert.ok(seen.line.includes("A pool can hold up to 15 accounts."), seen.line);
  assert.equal(seen.members, 15);
  assert.equal(seen.focused.id, "pool-full-notice");
  assert.ok(seen.says.includes("15 of 15"), seen.says);
});

test("a member who is not signed in keeps its note in the editor and in the pane and its count on the list", async () => {
  const seen = await onPools({ pools: "waiting", accounts: "waiting" }, async (page) => {
    const listed = await items(page);
    const pane = await say(page);
    await press(page, EDIT_MAIN);
    return { about: listed[0].about, pane, says: await say(page) };
  });
  const note = "1 account isn't signed in. Agents skip it until you sign in again.";
  assert.ok(seen.about.includes("2 ready, 1 not signed in"), seen.about);
  assert.ok(seen.pane.includes(note), seen.pane);
  assert.ok(seen.says.includes(note), seen.says);
});

test("Loading draws its skeletons under the title and in the account list", async () => {
  const seen = await onPools({ pools: "pending" }, async (page) => ({ top: await page.evaluate("Fixture.top()"), skeletons: await page.evaluate("Fixture.count('.skeleton')") }));
  assert.equal(seen.top?.heading, "Pools");
  assert.ok(seen.skeletons >= 4, String(seen.skeletons));
  const accounts = await onPools({ accounts: "pending" }, async (page) => {
    await press(page, ".set-head button", "New pool");
    return page.evaluate("Fixture.count('.skeleton')");
  });
  assert.ok(accounts >= 6, String(accounts));
});

test("Try again reads the pools a second time", async () => {
  const reads = await onPools({ pools: "error" }, async (page) => {
    await press(page, "button", "Try again");
    return page.evaluate("Fixture.commands()");
  });
  assert.equal(reads.filter((command) => command === "desktop_pool_summaries").length, 2, JSON.stringify(reads));
});

test("Account choice stays below a list of pools and only there", async () => {
  const listed = await onPools({}, say);
  assert.ok(listed.indexOf("Account choice") > listed.indexOf("Your pools"), listed);
  const none = await onPools({ pools: "empty" }, say);
  assert.ok(!none.includes("Account choice"), none);
});

test("Delete pool asks first, and a refused delete shows under the title", async () => {
  const seen = await onPools({}, async (page) => {
    await press(page, ITEM(2));
    await press(page, 'button[aria-label="Delete pool Spare"]');
    const asking = await page.evaluate("document.querySelector('dialog[open]')?.innerText ?? ''");
    const before = await page.evaluate("Fixture.commands()");
    await press(page, "dialog[open] button", "Delete pool");
    return { asking, before, alert: await page.evaluate("Fixture.alert()"), commands: await page.evaluate("Fixture.commands()") };
  });
  assert.match(seen.asking, /Delete Spare\?/);
  assert.match(seen.asking, /Its accounts stay in your list\. Only the pool goes away\./);
  assert.ok(!seen.before.includes("desktop_inspect_model_access"), JSON.stringify(seen.before));
  assert.ok(seen.commands.includes("desktop_inspect_model_access"), JSON.stringify(seen.commands));
  assert.match(seen.alert.words, /engine_failed/);
  assert.equal(seen.alert.belowTop, true, JSON.stringify(seen.alert));
});

const READY = "Get ready for agents";
const INSPECT = "desktop_inspect_model_access";

/** Reads a pool's Get ready button. Inputs: the page and the pool's name. Output: its visible
 * words and whether it is disabled. */
const readyState = (page, pool) => page.evaluate(`(() => {
  const found = document.querySelector('button[aria-label="${READY}: ${pool}"]');
  return { words: found?.textContent, disabled: found?.disabled };
})()`);

test("a double press on Get ready for agents runs once and shows it is working", async () => {
  const seen = await onPools({ hold: INSPECT }, async (page) => {
    await doublePress(page, `button[aria-label="${READY}: Main pool"]`);
    return { commands: await page.evaluate("Fixture.commands()"), button: await readyState(page, "Main pool") };
  });
  assert.equal(seen.commands.filter((command) => command === INSPECT).length, 1, JSON.stringify(seen.commands));
  assert.deepEqual(seen.button, { words: "Getting ready...", disabled: true });
});

test("while one pool gets ready, another pool's Get ready waits and keeps its word", async () => {
  const other = await onPools({ hold: INSPECT, pools: "keyed" }, async (page) => {
    await press(page, `button[aria-label="${READY}: Main pool"]`);
    await press(page, ITEM(2));
    return readyState(page, "Spare");
  });
  assert.deepEqual(other, { words: READY, disabled: true });
});

test("when the run ends Get ready for agents can be pressed again", async () => {
  const button = await onPools({}, async (page) => {
    await press(page, `button[aria-label="${READY}: Main pool"]`);
    return readyState(page, "Main pool");
  });
  assert.deepEqual(button, { words: READY, disabled: false });
});

test("Get ready for agents reaches the engine and says which account is not ready", async () => {
  const seen = await onPools({}, async (page) => {
    await press(page, 'button[aria-label="Get ready for agents: Main pool"]');
    return { commands: await page.evaluate("Fixture.commands()"), alert: await page.evaluate("Fixture.alert()") };
  });
  assert.ok(seen.commands.includes("desktop_inspect_model_access"), JSON.stringify(seen.commands));
  assert.match(seen.alert.words, /Account 0 isn't ready for agents yet\./);
});

for (const theme of ["light", "dark"]) {
  test(`the pool list and every row part read at 4.5 to 1 in ${theme}`, async () => {
    const seen = await onPools({ ranking: "held", limits: "mixed" }, async (page) => {
      const read = {};
      for (const selector of [".pool-item-count", ".pool-item.on .pool-item-label", ".pool-item-about", ".pool-pos", ".pool-row.held .pool-pos", ".pool-plan", ".pool-why", ".pool-row.held .pool-why", ".pool-why a", ".pool-limit.near", ".pool-limit.full", ".pool-limit-none", ".pool-held-head", ".pool-order p"]) {
        read[selector] = await page.evaluate(`Fixture.contrast(${JSON.stringify(selector)})`);
      }
      return read;
    }, 1440, theme);
    for (const [name, ratio] of Object.entries(seen)) assert.ok(ratio >= CONTRAST_MIN, `${name}: ${ratio}`);
  });

  test(`help and the changed mark read at 4.5 to 1 in ${theme}`, async () => {
    const seen = await onPools({}, async (page) => {
      const help = await page.evaluate('Fixture.contrast(".set-head p")');
      await press(page, EDIT_MAIN);
      await page.evaluate("document.getElementById('pool-name').focus()");
      await page.send("Input.insertText", { text: "x" });
      await page.evaluate("Fixture.settle(40)");
      return { help, mark: await page.evaluate('Fixture.contrast(".set-changed")') };
    }, 1180, theme);
    for (const [name, ratio] of Object.entries(seen)) assert.ok(ratio >= CONTRAST_MIN, `${name}: ${ratio}`);
  });
}
