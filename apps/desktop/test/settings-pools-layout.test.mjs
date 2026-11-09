import assert from "node:assert/strict";
import test from "node:test";
import { bundle } from "./browser.mjs";
import { CONTRAST_MIN, press, tabRunner, tabWalk } from "./settings-tab-steps.mjs";

const fixture = await bundle(new URL("./fixtures/settings-pools-layout.mjs", import.meta.url).pathname);
const onPools = tabRunner(fixture);
const EDIT_MAIN = 'button[aria-label="Edit pool Main pool"]';

const read = (page) => page.evaluate("Fixture.sections()");
const say = (page) => page.evaluate("Fixture.text()");

test("the list shows the title, its purpose and New pool, then Your pools with one line per pool", async () => {
  const seen = await onPools({}, async (page) => ({ top: await page.evaluate("Fixture.top()"), sections: await read(page) }));
  assert.deepEqual(seen.top?.heading, "Pools", JSON.stringify(seen.top));
  assert.match(seen.top.purpose, /A pool is a group of accounts for a project\. When one account runs out, agents move to the next one in the pool\./);
  assert.deepEqual(seen.top.actions, ["New pool"]);
  assert.deepEqual(seen.sections.map((section) => [section.heading, section.about]), [["Your pools", "2 pools"]]);
  const [main, spare] = seen.sections[0].rows;
  assert.equal(main.label, "Main pool");
  for (const part of ["3 ChatGPT", "3 of 15", "3 ready", "Used by 2 projects"]) assert.ok(main.help.includes(part), `${part} in ${main.help}`);
  assert.ok(main.controlLeft > main.labelRight, JSON.stringify(main));
  assert.deepEqual(main.visible, ["Get ready for agents", "Edit pool", "Delete pool"]);
  assert.deepEqual(main.buttons, ["Get ready for agents: Main pool", "Edit pool Main pool", "Delete pool Main pool"]);
  assert.deepEqual(spare.visible, ["Edit pool", "Delete pool"], "only a pool with keyed members can be set up for agents");
});

test("a 60-letter pool name wraps inside its label and leaves the actions alone", async () => {
  const [section] = await onPools({ pools: "long" }, read);
  const [row] = section.rows;
  assert.equal(row.label.length, 60);
  assert.equal(row.labelSpills, false, JSON.stringify(row));
  assert.ok(row.controlLeft >= row.labelRight, JSON.stringify(row));
  assert.deepEqual(row.visible, ["Edit pool", "Delete pool"]);
});

test("real Tab keys walk New pool, then each pool's actions in row order", async () => {
  const walked = await onPools({}, (page) => tabWalk(page, 6));
  const from = walked.indexOf("New pool");
  assert.notEqual(from, -1, JSON.stringify(walked));
  assert.deepEqual(walked.slice(from, from + 6), ["New pool", "Get ready for agents: Main pool", "Edit pool Main pool", "Delete pool Main pool", "Edit pool Spare", "Delete pool Spare"]);
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
    await press(page, ".settings-top button", "New pool");
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
    const says = await onPools(scene, async (page) => { await press(page, ".settings-top button", "New pool"); return say(page); });
    for (const part of expected) assert.ok(says.includes(part), `${part} in ${says}`);
  });
}

test("the account list keeps Provider, Selected only, search and their empty lines", async () => {
  const seen = await onPools({}, async (page) => {
    await press(page, ".settings-top button", "New pool");
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
    await press(page, ".settings-top button", "New pool");
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

test("a member who is not signed in keeps its note in the editor and its count on the list", async () => {
  const seen = await onPools({ pools: "waiting", accounts: "waiting" }, async (page) => {
    const list = await read(page);
    await press(page, EDIT_MAIN);
    return { help: list[0].rows[0].help, says: await say(page) };
  });
  assert.ok(seen.help.includes("2 ready, 1 not signed in"), seen.help);
  assert.ok(seen.says.includes("1 account isn't signed in. Agents skip it until you sign in again."), seen.says);
});

test("Loading draws its skeletons under the title and in the account list", async () => {
  const seen = await onPools({ pools: "pending" }, async (page) => ({ top: await page.evaluate("Fixture.top()"), skeletons: await page.evaluate("Fixture.count('.skeleton')") }));
  assert.equal(seen.top?.heading, "Pools");
  assert.ok(seen.skeletons >= 4, String(seen.skeletons));
  const accounts = await onPools({ accounts: "pending" }, async (page) => {
    await press(page, ".settings-top button", "New pool");
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

test("Get ready for agents reaches the engine and says which account is not ready", async () => {
  const seen = await onPools({}, async (page) => {
    await press(page, 'button[aria-label="Get ready for agents: Main pool"]');
    return { commands: await page.evaluate("Fixture.commands()"), alert: await page.evaluate("Fixture.alert()") };
  });
  assert.ok(seen.commands.includes("desktop_inspect_model_access"), JSON.stringify(seen.commands));
  assert.match(seen.alert.words, /Account 0 isn't ready for agents yet\./);
});

for (const theme of ["light", "dark"]) {
  test(`help and the changed mark read at 4.5 to 1 in ${theme}`, async () => {
    const seen = await onPools({}, async (page) => {
      const help = await page.evaluate('Fixture.contrast(".set-help")');
      await press(page, EDIT_MAIN);
      await page.evaluate("document.getElementById('pool-name').focus()");
      await page.send("Input.insertText", { text: "x" });
      await page.evaluate("Fixture.settle(40)");
      return { help, mark: await page.evaluate('Fixture.contrast(".set-changed")') };
    }, 1180, theme);
    for (const [name, ratio] of Object.entries(seen)) assert.ok(ratio >= CONTRAST_MIN, `${name}: ${ratio}`);
  });
}
