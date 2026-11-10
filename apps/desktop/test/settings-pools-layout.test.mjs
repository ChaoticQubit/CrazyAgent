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
const ACCOUNTS_READ = "desktop_list_accounts";

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

test("real Tab keys walk New pool, each pool in the list, the app's order, then the selected pool's actions", async () => {
  const walked = await onPools({}, (page) => tabWalk(page, 7));
  const from = walked.indexOf("New pool");
  assert.notEqual(from, -1, JSON.stringify(walked));
  const names = walked.slice(from, from + 7);
  assert.match(names[1], /^Main pool/);
  assert.match(names[2], /^Spare/);
  assert.equal(names[3], "Use it or lose it");
  assert.deepEqual(names.slice(4), ["Get ready for agents: Main pool", "Edit pool Main pool", "Delete pool Main pool"]);
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
