import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { bundle } from "./browser.mjs";
import { doublePress, CONTRAST_MIN, press, tabRunner, tabWalk } from "./settings-tab-steps.mjs";

const fixture = await bundle(new URL("./fixtures/settings-pools-layout.mjs", import.meta.url).pathname);
const onPools = tabRunner(fixture);

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
const ACCOUNTS_READ = "desktop_list_accounts";
const EXTRAS_READ = "desktop_usage_extras";
const SOURCE = '[role="radiogroup"][aria-label="Claude free resets"] [role="radio"]';

test("the screen shows the title and purpose, Your pools with New pool on the left, the first pool selected on the right, and Free resets below", async () => {
  const seen = await onPools({}, async (page) => ({ top: await page.evaluate("Fixture.top()"), sections: await read(page), items: await items(page), cap: await page.evaluate("Fixture.wordsOf('.pool-cap')") }));
  assert.deepEqual(seen.top?.heading, "Pools", JSON.stringify(seen.top));
  assert.match(seen.top.purpose, /A pool is a group of accounts for a project\. When one account runs out, agents move to the next one in the pool\./);
  assert.deepEqual(seen.top.actions, []);
  assert.deepEqual(seen.sections.map((section) => [section.heading, section.actions]), [["Your pools", ["New pool"]], ["Main pool", ["Get ready for agents", "Delete pool"]], ["Free resets", []]]);
  assert.equal(seen.sections[1].about, "Used by 2 projects. 3 ready. Shown on each project's pool picker.");
  assert.deepEqual(seen.items.map((item) => [item.name, item.count, item.about, item.current]), [["Main pool", "3 of 15", "3 ready. Used by 2 projects.", "true"], ["Spare", "1 of 15", "1 ready. Not used.", null]]);
  assert.equal(seen.cap, "3 of 15Add account");
});

test("pressing a pool selects it and shows its own accounts, actions and order, and the focus stays on it", async () => {
  const seen = await onPools({}, async (page) => {
    await press(page, ITEM(2));
    return { items: await items(page), sections: await read(page), rows: await rows(page), order: await page.evaluate("Fixture.wordsOf('.pool-order p')"), head: await page.evaluate("Fixture.wordsOf('.pool-order-head')"), focused: await page.evaluate("Fixture.focused()") };
  });
  assert.deepEqual(seen.items.map((item) => item.current), [null, "true"]);
  assert.equal(seen.sections[1].heading, "Spare");
  assert.deepEqual(seen.sections[1].actions, ["Delete pool"], "only a pool with keyed members can be set up for agents");
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
  assert.deepEqual(seen.sections[1].actions, ["Delete pool"]);
  assert.equal(seen.panes.sideways, false, JSON.stringify(seen.panes));
});

test("real Tab keys walk New pool, each pool in the list, the app's order, then the selected pool's actions", async () => {
  const walked = await onPools({}, (page) => tabWalk(page, 8));
  const from = walked.indexOf("New pool");
  assert.notEqual(from, -1, JSON.stringify(walked));
  const names = walked.slice(from, from + 8);
  assert.match(names[1], /^Main pool/);
  assert.match(names[2], /^Spare/);
  assert.equal(names[3], "Use it or lose it");
  assert.deepEqual(names.slice(4), ["Keep start account", "Pool name", "Get ready for agents: Main pool", "Delete pool Main pool"]);
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

test("each row says its free resets and credits under its usage, and an account with no usage read still gets its line", async () => {
  const seen = await onPools({}, rows);
  assert.deepEqual(seen.map((row) => [row.name, row.extras]), [
    ["Account 2", "2 free resets (weekly) \u00b7 Credits: $4.20 left"],
    ["Account 0", "No free resets \u00b7 Credits: not reported by Codex"],
    ["Account 1", "Free resets and credits: not reported by Codex"],
  ]);
  assert.equal(seen[2].none, "No usage read yet");
  for (const row of seen) assert.doesNotMatch(row.extras, /(^|\D)0 free|: 0\b/, "nothing missing reads as 0");
});

test("a row says where to spend when the provider spends elsewhere", async () => {
  const seen = await onPools({ extras: "spend" }, rows);
  assert.equal(seen[0].extras, "2 free resets \u00b7 Credits: not reported by Codex \u00b7 Spend at chatgpt.com");
});

test("Pools asks for the detail rows on every read while it is open", async () => {
  const asked = await onPools({}, async (page) => {
    const first = await page.evaluate("Fixture.extrasDetail()");
    await page.evaluate("Fixture.fireTimer(60000)");
    return { first, later: await page.evaluate("Fixture.extrasDetail()") };
  });
  assert.ok(asked.first.length >= 1, JSON.stringify(asked));
  assert.ok(asked.later.length > asked.first.length, JSON.stringify(asked));
  assert.ok(asked.later.every((detail) => detail === true), JSON.stringify(asked));
});

test("each Claude free resets source picked reads every row's free resets again once the engine kept it, so a Claude row never waits the minute reread to follow it (DEC-154 addendum)", async () => {
  const seen = await onPools({}, async (page) => {
    const reads = [await commandCount(page, EXTRAS_READ)];
    for (const option of ["Through Claude Code", "As Claude Code", "Don't read"]) {
      await press(page, SOURCE, option);
      reads.push(await commandCount(page, EXTRAS_READ));
    }
    return { reads, kept: await page.evaluate("Fixture.spendSettingsSent()") };
  });
  assert.deepEqual(seen.kept, ["through_claude_code", "as_claude_code", "off"].map((value) => ({ field: "resets_source", value })));
  assert.deepEqual(seen.reads.slice(1).map((reads, at) => reads - seen.reads[at]), [1, 1, 1], JSON.stringify(seen));
});

test("a failed extras read keeps each row's usage and shows no made-up line", async () => {
  const seen = await onPools({ extras: "error" }, rows);
  assert.deepEqual(seen.map((row) => row.extras), ["", "", ""]);
  assert.deepEqual(seen[0].limits, [["Weekly62% used", "ok"], ["5-hour18% used", "ok"]]);
});

test("held accounts come after the held line with no number, in the ranking's order", async () => {
  const seen = await onPools({ ranking: "held" }, async (page) => ({ rows: await rows(page), head: await page.evaluate("Fixture.heldHead()") }));
  assert.deepEqual(seen.rows.map((row) => [row.number, row.spoken, row.name, row.held]), [["1", "Position 1", "Account 2", false], ["-", "Held", "Account 1", true], ["-", "Held", "Account 0", true]]);
  assert.deepEqual(seen.head, { words: "Held now, in pool order. Each one rejoins the order when its reason ends.", between: true });
  assert.equal(seen.rows[2].why, "Never use. You pinned it.");
});

test("an account one project holds keeps its place and says on its own line which project holds it until when", async () => {
  const seen = await onPools({ ranking: "project-held" }, rows);
  assert.deepEqual(seen.map((row) => [row.number, row.name, row.held]), [["1", "Account 2", false], ["2", "Account 0", false], ["-", "Account 1", true]]);
  assert.equal(seen[0].why, "You ranked it 1.");
  assert.equal(seen[0].holds.length, 1);
  assert.match(seen[0].holds[0], /^Held for Meals for two until (\d{2}:\d{2}|[A-Z][a-z]{2} \d{2}:\d{2})$/);
  assert.deepEqual([seen[1].holds, seen[2].holds], [[], []]);
});

test("a sign-in that needs a check is held with no number, says why and links to its own sign-in", async () => {
  const seen = await onPools({ ranking: "unconfirmed" }, rows);
  assert.deepEqual(seen.map((row) => [row.number, row.spoken, row.name, row.held]), [["1", "Position 1", "Account 2", false], ["-", "Held", "Account 1", true]]);
  assert.equal(seen[1].why, "Its sign-in needs a check before agents can use it. Sign in");
  assert.deepEqual(seen[1].link, { href: "#/settings/accounts?action=sign_in&account=acct-1", label: "Sign in Account 1" });
  assert.equal(seen[0].link, null);
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

test("the row's cap on limit rows is the engine's", () => {
  const cap = (path, shape) => readFileSync(new URL(path, import.meta.url), "utf8").match(shape)?.[1];
  const engine = cap("../../../crates/crazyagent/src/usage/report.rs", /pub const LIMITS_MAX: usize = (\d+);/u);
  assert.ok(engine, "the engine still names its cap");
  assert.equal(cap("../src/screens/settings/pools.tsx", /^const LIMITS_MAX = (\d+);/mu), engine);
});

for (const [scene, words, absent] of [
  [{ ranking: "missing" }, ["Account order isn't ready yet"], "No account can take work now."],
  [{ ranking: "error" }, ["Couldn't load the account order.", "Try again"], "No account can take work now."],
  [{ ranking: "bare" }, ["No accounts to rank yet."], "No account can take work now."],
  [{ limits: "error" }, ["Couldn't read usage limits."], "No usage read yet"],
  [{ limits: "pending" }, ["Account 2"], "No usage read yet"],
  [{ limits: "missing" }, ["Account 2"], "No usage read yet"],
  [{ limits: "capped" }, ["Account 2"], "No usage read yet"],
  [{ limits: "uncapped" }, ["Account 2", "No usage read yet"], "Couldn't read usage limits."],
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
  assert.ok(says.includes("No accounts yet. Use Add account to put one in this pool."), says);
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

test("the page reads the ranking, the limits, the pools and the accounts again once a minute", async () => {
  const seen = await onPools({}, async (page) => {
    const delays = await page.evaluate("Fixture.timerDelays()");
    const reads = async () => [await commandCount(page, RANKING), await commandCount(page, USAGE), await commandCount(page, POOLS_READ), await commandCount(page, ACCOUNTS_READ)];
    const before = await reads();
    await page.evaluate("Fixture.fireTimer(60000)");
    return { delays, before, after: await reads() };
  });
  assert.equal(seen.delays.filter((delay) => delay === 60000).length, 1, JSON.stringify(seen.delays));
  assert.deepEqual(seen.after, seen.before.map((count) => count + 1));
});

test("an account that signs out while the page is open changes its badge, the left line and the note at the next tick", async () => {
  const note = "1 account isn't signed in. Agents skip it until you sign in again.";
  const seen = await onPools({}, async (page) => {
    const before = { rows: await rows(page), items: await items(page), says: await pane(page) };
    await page.evaluate(`Fixture.changeScene({ pools: "waiting", accounts: "waiting", ranking: "held" })`);
    await page.evaluate("Fixture.fireTimer(60000)");
    return { before, rows: await rows(page), items: await items(page), says: await pane(page) };
  });
  assert.deepEqual(seen.before.rows.find((row) => row.name === "Account 1").badges, ["Signed in"]);
  assert.equal(seen.before.items[0].about, "3 ready. Used by 2 projects.");
  assert.ok(!seen.before.says.includes(note), seen.before.says);
  const row = seen.rows.find((item) => item.name === "Account 1");
  assert.deepEqual(row.badges, ["Not signed in"]);
  assert.match(row.why, /^Not signed in\. Agents skip it until you sign in again\./);
  assert.equal(seen.items[0].about, "2 ready, 1 not signed in. Used by 2 projects.");
  assert.ok(seen.says.includes(note), seen.says);
});

test("both order lists keep their list role for a screen reader", async () => {
  const seen = await onPools({ ranking: "held" }, (page) => page.evaluate(`[document.querySelector("ol.pool-rows").getAttribute("role"), document.querySelector("ul.pool-rows").getAttribute("role")]`));
  assert.deepEqual(seen, ["list", "list"]);
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

test("a member who is not signed in keeps its note in the pane and its count on the list", async () => {
  const seen = await onPools({ pools: "waiting", accounts: "waiting" }, async (page) => ({ about: (await items(page))[0].about, pane: await say(page) }));
  const note = "1 account isn't signed in. Agents skip it until you sign in again.";
  assert.ok(seen.about.includes("2 ready, 1 not signed in"), seen.about);
  assert.ok(seen.pane.includes(note), seen.pane);
});

test("Loading draws its skeletons under the title and in the account list", async () => {
  const seen = await onPools({ pools: "pending" }, async (page) => ({ top: await page.evaluate("Fixture.top()"), skeletons: await page.evaluate("Fixture.count('.skeleton')") }));
  assert.equal(seen.top?.heading, "Pools");
  assert.ok(seen.skeletons >= 4, String(seen.skeletons));
  const accounts = await onPools({ accounts: "pending" }, async (page) => {
    await press(page, ".set-head button", "New pool");
    return page.evaluate("Fixture.count('.skeleton')");
  });
  assert.ok(accounts >= 3, String(accounts));
});

test("Try again reads the pools a second time", async () => {
  const reads = await onPools({ pools: "error" }, async (page) => {
    await press(page, "button", "Try again");
    return page.evaluate("Fixture.commands()");
  });
  assert.equal(reads.filter((command) => command === "desktop_pool_summaries").length, 2, JSON.stringify(reads));
});

test("How every pool picks an account stays under Your pools and only with a list of pools", async () => {
  const listed = await onPools({}, say);
  assert.ok(listed.indexOf("How every pool picks an account") > listed.indexOf("Your pools"), listed);
  const none = await onPools({ pools: "empty" }, say);
  assert.ok(!none.includes("How every pool picks an account"), none);
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
      for (const selector of [".pool-item-count", ".pool-item.on .pool-item-label", ".pool-item-about", ".pool-pos", ".pool-row.held .pool-pos", ".pool-plan", ".pool-why", ".pool-row.held .pool-why", ".pool-why a", ".pool-limit.near", ".pool-limit.full", ".pool-limit-none", ".pool-extras", ".pool-held-head", ".pool-order p"]) {
        read[selector] = await page.evaluate(`Fixture.contrast(${JSON.stringify(selector)})`);
      }
      return read;
    }, 1440, theme);
    for (const [name, ratio] of Object.entries(seen)) assert.ok(ratio >= CONTRAST_MIN, `${name}: ${ratio}`);
  });

  test(`help reads at 4.5 to 1 in ${theme}`, async () => {
    const help = await onPools({}, (page) => page.evaluate('Fixture.contrast(".set-head p")'), 1180, theme);
    assert.ok(help >= CONTRAST_MIN, String(help));
  });
}
