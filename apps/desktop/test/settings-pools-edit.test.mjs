import assert from "node:assert/strict";
import test from "node:test";
import { bundle } from "./browser.mjs";
import { CONTRAST_MIN, press, tabRunner } from "./settings-tab-steps.mjs";

const fixture = await bundle(new URL("./fixtures/settings-pools-layout.mjs", import.meta.url).pathname);
const onPools = tabRunner(fixture);
const LIVE = { pools: "live", ranking: "pools" };
const THREE = { pools: "live-new", ranking: "pools" };
const POOL_FULL = "A pool can hold up to 15 accounts. Remove one to add another.";
const LAST_ACCOUNT = "A pool needs at least one account. Add another account first, or delete the pool.";
const DRAFT_EMPTY = "No accounts yet. This pool is created with its first account.";
const ADD_ACCOUNT = '[data-control="add-account"]';
const ADD = (name) => `button[aria-label="Add ${name}"]`;
const REMOVE = (name) => `button[aria-label="Remove ${name}"]`;
const ITEM = (at) => `.pool-items .pool-item:nth-child(${at})`;
const INSPECT = "desktop_inspect_model_access";
const NEW = ".set-head button";
const rows = (page) => page.evaluate("Fixture.orderRows()");
const rowNames = async (page) => (await rows(page)).map((row) => row.name);
const items = (page) => page.evaluate("Fixture.poolItems()");
const edits = (page) => page.evaluate("Fixture.poolEdits()");
const readied = (page) => page.evaluate("Fixture.readied()");
const words = (page, selector) => page.evaluate(`Fixture.wordsOf(${JSON.stringify(selector)})`);
const saved = (page) => words(page, ".pool-saved");
const alert = async (page) => (await page.evaluate("Fixture.alert()"))?.words ?? "";
const focusAt = (page) => page.evaluate("Fixture.focusPlace()");
const addNames = (page) => page.evaluate("Fixture.addRows()");
const nameField = (page) => page.evaluate("document.getElementById('pool-name').value");
const openCount = (page) => page.evaluate("Fixture.count('dialog[open]')");
const heading = (page) => page.evaluate("Fixture.sections()[1].heading");
const members = (edit) => edit.pool.members;

/** Presses and lets go of one real key. Inputs: the page, the key and its key code. */
async function pressKey(page, key, code) {
  for (const type of ["keyDown", "keyUp"]) await page.send("Input.dispatchKeyEvent", { type, key, code: key, windowsVirtualKeyCode: code });
  await page.evaluate("Fixture.settle(80)");
}

/** Replaces the pool name's words with real input. Inputs: the page and the words. */
async function typeName(page, text) {
  await press(page, "#pool-name");
  await page.evaluate("document.getElementById('pool-name').select()");
  if (text === "") await pressKey(page, "Backspace", 8);
  else await page.send("Input.insertText", { text });
  await page.evaluate("Fixture.settle(30)");
}

test("Add account opens the panel with search and provider, and Done closes it and gives the focus back", async () => {
  const seen = await onPools(LIVE, async (page) => {
    const state = () => page.evaluate(`({ expanded: document.querySelector('${ADD_ACCOUNT}').getAttribute("aria-expanded"), panel: Fixture.count(".pool-add"), search: Fixture.count(".pool-add input[type=search]"), provider: Fixture.count("#pool-provider") })`);
    const closed = await state();
    await press(page, ADD_ACCOUNT);
    const open = await state();
    await press(page, ".pool-add button", "Done");
    return { closed, open, done: await state(), focus: await focusAt(page) };
  });
  assert.deepEqual(seen.closed, { expanded: "false", panel: 0, search: 0, provider: 0 });
  assert.deepEqual(seen.open, { expanded: "true", panel: 1, search: 1, provider: 1 });
  assert.deepEqual(seen.done, { expanded: "false", panel: 0, search: 0, provider: 0 });
  assert.equal(seen.focus.words, "Add account");
});

test("the panel lists only accounts outside the pool, each with its state", async () => {
  const seen = await onPools(LIVE, async (page) => {
    await press(page, ADD_ACCOUNT);
    return { names: await addNames(page), line: await words(page, ".pool-add-row") };
  });
  for (const member of ["Account 0", "Account 1", "Account 2"]) assert.ok(!seen.names.includes(member), JSON.stringify(seen.names));
  assert.ok(seen.names.includes("Account 3"), JSON.stringify(seen.names));
  assert.match(seen.line, /^Account 3\s*ChatGPT\. Signed in\.\s*Add$/);
});

test("a long account list stays a short window in the panel", async () => {
  const seen = await onPools(LIVE, async (page) => {
    await press(page, ADD_ACCOUNT);
    return { drawn: (await addNames(page)).length, height: await page.evaluate("document.querySelector('.pool-add-list').getBoundingClientRect().height") };
  });
  assert.ok(seen.drawn > 0, String(seen.drawn));
  assert.ok(seen.height <= 176, String(seen.height));
});

test("the name field is the pool's visible title and the heading stays for a screen reader", async () => {
  const seen = await onPools(LIVE, async (page) => ({ field: await nameField(page), heading: await heading(page), width: await page.evaluate("document.querySelector('.pool-detail h3').getBoundingClientRect().width") }));
  assert.equal(seen.field, "Main pool");
  assert.equal(seen.heading, "Main pool");
  assert.ok(seen.width <= 1, String(seen.width));
});

for (const width of [1180, 900]) {
  test(`at ${width} px the open panel moves nothing sideways`, async () => {
    const panes = await onPools(LIVE, async (page) => {
      await press(page, ADD_ACCOUNT);
      return page.evaluate("Fixture.panes()");
    }, width);
    assert.equal(panes.sideways, false, JSON.stringify(panes));
  });
}

for (const theme of ["light", "dark"]) {
  test(`the panel's words and the name field read at 4.5 to 1 in ${theme}`, async () => {
    const seen = await onPools(LIVE, async (page) => {
      await press(page, ADD_ACCOUNT);
      const read = {};
      for (const selector of [".pool-add-row .small.secondary", ".pool-add-row .ellipsis", "#pool-name"]) read[selector] = await page.evaluate(`Fixture.contrast(${JSON.stringify(selector)})`);
      return read;
    }, 1440, theme);
    for (const [name, ratio] of Object.entries(seen)) assert.ok(ratio >= CONTRAST_MIN, `${name}: ${ratio}`);
  });
}

test("search narrows the panel, and No accounts match says when nothing is left", async () => {
  const seen = await onPools(LIVE, async (page) => {
    await press(page, ADD_ACCOUNT);
    await page.evaluate("document.querySelector('.pool-add input[type=search]').focus()");
    await page.send("Input.insertText", { text: "Account 17" });
    await page.evaluate("Fixture.settle(40)");
    const found = await addNames(page);
    await page.send("Input.insertText", { text: " nothing" });
    await page.evaluate("Fixture.settle(40)");
    return { found, empty: await words(page, ".pool-add") };
  });
  assert.deepEqual(seen.found, ["Account 17"]);
  assert.ok(seen.empty.includes("No accounts match."), seen.empty);
});

test("Provider narrows the panel to that provider's accounts", async () => {
  const seen = await onPools({ ...LIVE, accounts: "mixed" }, async (page) => {
    await press(page, ADD_ACCOUNT);
    await page.evaluate("Fixture.choose('#pool-provider', 'claude')");
    return addNames(page);
  });
  assert.deepEqual(seen, ["Account 16", "Account 17"]);
});

test("Add puts the account in at once, says so and the row appears in its calculated place", async () => {
  const seen = await onPools(LIVE, async (page) => {
    await press(page, ADD_ACCOUNT);
    await press(page, ADD("Account 3"));
    return { edits: await edits(page), said: await saved(page), names: await rowNames(page), cap: await words(page, ".capacity-count"), left: await addNames(page) };
  });
  assert.deepEqual(seen.edits.map((edit) => [edit.kind, edit.pool.id, members(edit)]), [["replace_pool", "pool-main", ["acct-0", "acct-1", "acct-2", "acct-3"]]]);
  assert.equal(seen.said, "Added Account 3.");
  assert.deepEqual(seen.names, ["Account 0", "Account 1", "Account 2", "Account 3"]);
  assert.equal(seen.cap, "4 of 15");
  assert.ok(!seen.left.includes("Account 3"), JSON.stringify(seen.left));
});

test("Add takes the keyboard to the search once the added account leaves the list", async () => {
  const focus = await onPools(LIVE, async (page) => {
    await press(page, ADD_ACCOUNT);
    await press(page, ADD("Account 3"));
    return focusAt(page);
  });
  assert.equal(focus.tag, "INPUT");
});

test("adding a key account says it is getting ready, shows the row first and then readies it", async () => {
  const seen = await onPools({ ...LIVE, accounts: "keyed" }, async (page) => {
    await page.evaluate(`Fixture.gate("desktop_configure_account_profile")`);
    await press(page, ADD_ACCOUNT);
    await press(page, ADD("Account 5"));
    const waiting = { said: await saved(page), names: await rowNames(page), readied: await readied(page) };
    await page.evaluate("Fixture.openGate()");
    return { waiting, after: await readied(page) };
  });
  assert.equal(seen.waiting.said, "Added Account 5. Getting it ready for agents.");
  assert.ok(seen.waiting.names.includes("Account 5"), JSON.stringify(seen.waiting.names));
  assert.deepEqual(seen.waiting.readied, []);
  assert.deepEqual(seen.after, ["acct-5"]);
});

test("a key account that cannot be readied says so and stays in the pool", async () => {
  const seen = await onPools({ ...LIVE, accounts: "keyed", ready: "refuse" }, async (page) => {
    await press(page, ADD_ACCOUNT);
    await press(page, ADD("Account 5"));
    return { alert: await alert(page), names: await rowNames(page) };
  });
  assert.match(seen.alert, /^Account 5 isn't ready for agents yet\. /);
  assert.ok(seen.names.includes("Account 5"), JSON.stringify(seen.names));
});

test("a key account whose Add the engine refuses is not readied", async () => {
  const seen = await onPools({ ...LIVE, accounts: "keyed", refuseEdit: "pool_name_bounds" }, async (page) => {
    await press(page, ADD_ACCOUNT);
    await press(page, ADD("Account 5"));
    return { readied: await readied(page), says: await alert(page) };
  });
  assert.deepEqual(seen.readied, []);
  assert.match(seen.says, /Use a pool name of 1 to 60 characters on one line\./);
});

test("a subscription account is added without readying anything", async () => {
  const seen = await onPools(LIVE, async (page) => {
    await press(page, ADD_ACCOUNT);
    await press(page, ADD("Account 3"));
    return readied(page);
  });
  assert.deepEqual(seen, []);
});

test("a full pool shows the limit in the panel and a 16th Add is refused with it and sends nothing", async () => {
  const seen = await onPools({ pools: "live-full", ranking: "pools" }, async (page) => {
    await press(page, ADD_ACCOUNT);
    const note = await words(page, ".pool-add");
    const unavailable = await page.evaluate(`document.querySelector('${ADD("Account 15").replaceAll('"', '\\"')}').getAttribute("aria-disabled")`);
    await press(page, ADD("Account 15"));
    return { note, unavailable, alert: await alert(page), edits: await edits(page) };
  });
  assert.ok(seen.note.includes(POOL_FULL), seen.note);
  assert.equal(seen.unavailable, "true");
  assert.equal(seen.alert, POOL_FULL);
  assert.deepEqual(seen.edits, []);
});

test("two quick Adds on a pool of 14 keep the first and refuse the second with the limit", async () => {
  const seen = await onPools({ pools: "live-14", ranking: "pools" }, async (page) => {
    await press(page, ADD_ACCOUNT);
    await page.evaluate(`Fixture.gate("${INSPECT}")`);
    await press(page, ADD("Account 14"));
    await press(page, ADD("Account 15"));
    await page.evaluate("Fixture.openGate()");
    await page.evaluate("Fixture.settle(120)");
    return { edits: await edits(page), alert: await alert(page), cap: await words(page, ".capacity-count") };
  });
  assert.equal(seen.edits.length, 1, JSON.stringify(seen.edits));
  assert.equal(members(seen.edits[0]).at(-1), "acct-14");
  assert.equal(seen.alert, POOL_FULL);
  assert.equal(seen.cap, "15 of 15");
});

test("Remove asks nothing, takes the account out, says it stays in the list and moves the focus to Add account", async () => {
  const seen = await onPools(LIVE, async (page) => {
    await press(page, REMOVE("Account 1"));
    return { dialogs: await openCount(page), edits: await edits(page), said: await saved(page), names: await rowNames(page), focus: await focusAt(page) };
  });
  assert.equal(seen.dialogs, 0);
  assert.deepEqual(seen.edits.map(members), [["acct-0", "acct-2"]]);
  assert.equal(seen.said, "Removed Account 1. It stays in your list.");
  assert.deepEqual(seen.names, ["Account 0", "Account 2"]);
  assert.equal(seen.focus.words, "Add account");
});

test("removing the last account shows the engine's sentence and keeps the row", async () => {
  const seen = await onPools({ pools: "live-one", ranking: "pools" }, async (page) => {
    await press(page, REMOVE("Account 3"));
    return { alert: await alert(page), names: await rowNames(page), said: await saved(page) };
  });
  assert.equal(seen.alert, LAST_ACCOUNT);
  assert.deepEqual(seen.names, ["Account 3"]);
  assert.equal(seen.said, "");
});

test("New pool appears at once in the left list, selected, with the add panel open and nothing sent", async () => {
  const seen = await onPools(LIVE, async (page) => {
    await press(page, NEW, "New pool");
    return { items: await items(page), panel: await page.evaluate("Fixture.count('.pool-add')"), name: await nameField(page), edits: await edits(page), cap: await words(page, ".capacity-count") };
  });
  assert.deepEqual(seen.items.map((item) => [item.name, item.count, item.current]), [["Main pool", "3 of 15", null], ["Spare", "1 of 15", null], ["New pool", "0 of 15", "true"]]);
  assert.equal(seen.panel, 1);
  assert.equal(seen.name, "New pool");
  assert.deepEqual(seen.edits, []);
  assert.equal(seen.cap, "0 of 15");
});

test("a draft takes the first free name", async () => {
  const name = await onPools({ pools: "live-new", ranking: "pools" }, async (page) => {
    await press(page, NEW, "New pool");
    return nameField(page);
  });
  assert.equal(name, "New pool 3");
});

test("an empty draft says it is created with its first account and offers no delete or order", async () => {
  const seen = await onPools(LIVE, async (page) => {
    await press(page, NEW, "New pool");
    return { empty: await words(page, ".pool-empty"), actions: await page.evaluate("Fixture.sections()[1].actions"), order: await page.evaluate("Fixture.count('.pool-order')") };
  });
  assert.equal(seen.empty, DRAFT_EMPTY);
  assert.deepEqual(seen.actions, []);
  assert.equal(seen.order, 0);
});

test("picking another pool drops an empty draft, asks nothing and sends nothing", async () => {
  const seen = await onPools(LIVE, async (page) => {
    await press(page, NEW, "New pool");
    await press(page, ITEM(2));
    return { items: await items(page), dialogs: await openCount(page), edits: await edits(page), panel: await page.evaluate("Fixture.count('.pool-add')") };
  });
  assert.deepEqual(seen.items.map((item) => [item.name, item.current]), [["Main pool", null], ["Spare", "true"]]);
  assert.equal(seen.dialogs, 0);
  assert.deepEqual(seen.edits, []);
  assert.equal(seen.panel, 0);
});

test("leaving the page drops an empty draft and sends nothing", async () => {
  const seen = await onPools(LIVE, async (page) => {
    await press(page, NEW, "New pool");
    await page.evaluate("Fixture.unmount()");
    return { edits: await edits(page), commands: await page.evaluate("Fixture.commands()") };
  });
  assert.deepEqual(seen.edits, []);
  assert.ok(!seen.commands.includes("desktop_model_access_page"), JSON.stringify(seen.commands));
});

test("the name field edits the draft and its left row follows while nothing is sent", async () => {
  const seen = await onPools(LIVE, async (page) => {
    await press(page, NEW, "New pool");
    await typeName(page, "Fresh");
    return { items: await items(page), edits: await edits(page), section: await heading(page) };
  });
  assert.equal(seen.items.at(-1).name, "Fresh");
  assert.deepEqual(seen.edits, []);
  assert.equal(seen.section, "Fresh");
});

test("the first Add creates the pool with the draft's name, selects it and keeps the panel open", async () => {
  const seen = await onPools(LIVE, async (page) => {
    await press(page, NEW, "New pool");
    await typeName(page, "Fresh");
    await press(page, ADD("Account 4"));
    return { edits: await edits(page), said: await saved(page), items: await items(page), names: await rowNames(page), panel: await page.evaluate("Fixture.count('.pool-add')") };
  });
  assert.deepEqual(seen.edits.map((edit) => [edit.kind, edit.pool.name, members(edit)]), [["create_pool", "Fresh", ["acct-4"]]]);
  assert.equal(seen.said, "Fresh created.");
  assert.deepEqual(seen.items.map((item) => [item.name, item.current]), [["Main pool", null], ["Spare", null], ["Fresh", "true"]]);
  assert.deepEqual(seen.names, ["Account 4"]);
  assert.equal(seen.panel, 1);
});

test("a first Add with the name left alone says New pool created", async () => {
  const said = await onPools(LIVE, async (page) => {
    await press(page, NEW, "New pool");
    await press(page, ADD("Account 4"));
    return saved(page);
  });
  assert.equal(said, "New pool created.");
});

test("two quick Adds on a draft send one create and then add to it", async () => {
  const seen = await onPools(LIVE, async (page) => {
    await press(page, NEW, "New pool");
    await page.evaluate(`Fixture.gate("desktop_model_access_page")`);
    await press(page, ADD("Account 4"));
    await press(page, ADD("Account 5"));
    await page.evaluate("Fixture.openGate()");
    await page.evaluate("Fixture.settle(120)");
    return { edits: await edits(page), items: await items(page), names: await rowNames(page) };
  });
  assert.deepEqual(seen.edits.map((edit) => [edit.kind, members(edit)]), [["create_pool", ["acct-4"]], ["replace_pool", ["acct-4", "acct-5"]]]);
  assert.equal(seen.items.length, 3, JSON.stringify(seen.items));
  assert.deepEqual(seen.names, ["Account 4", "Account 5"]);
});

test("pressing Add on one account twice on a draft puts it in once", async () => {
  const seen = await onPools(LIVE, async (page) => {
    await press(page, NEW, "New pool");
    await page.evaluate(`{ const add = document.querySelector('${ADD("Account 3")}'); add.click(); add.click(); }`);
    await page.evaluate("Fixture.settle(120)");
    return { edits: await edits(page), rows: await rowNames(page), says: await alert(page) };
  });
  assert.deepEqual(seen.edits.map((edit) => [edit.kind, members(edit)]), [["create_pool", ["acct-3"]]]);
  assert.deepEqual(seen.rows, ["Account 3"]);
  assert.equal(seen.says, "", "the second press is no refusal");
});

test("a refused first Add keeps the draft and shows the engine's sentence", async () => {
  const seen = await onPools({ ...LIVE, refuseEdit: "pool_name_bounds" }, async (page) => {
    await press(page, NEW, "New pool");
    await press(page, ADD("Account 4"));
    return { alert: await alert(page), items: await items(page), name: await nameField(page), empty: await words(page, ".pool-empty") };
  });
  assert.equal(seen.alert, "Use a pool name of 1 to 60 characters on one line.");
  assert.deepEqual(seen.items.at(-1).current, "true");
  assert.equal(seen.name, "New pool");
  assert.equal(seen.empty, DRAFT_EMPTY);
});

test("pressing New pool on an open draft keeps that one draft", async () => {
  const seen = await onPools(LIVE, async (page) => {
    await press(page, NEW, "New pool");
    await typeName(page, "Fresh");
    await press(page, NEW, "New pool");
    return { items: await items(page), name: await nameField(page) };
  });
  assert.equal(seen.items.length, 3, JSON.stringify(seen.items));
  assert.equal(seen.name, "Fresh");
});

test("New pool from the empty state opens a draft", async () => {
  const seen = await onPools({ pools: "live-empty", ranking: "pools" }, async (page) => {
    await press(page, "button", "New pool");
    return { items: await items(page), panel: await page.evaluate("Fixture.count('.pool-add')"), empty: await words(page, ".pool-empty") };
  });
  assert.deepEqual(seen.items.map((item) => [item.name, item.current]), [["New pool", "true"]]);
  assert.equal(seen.panel, 1);
  assert.equal(seen.empty, DRAFT_EMPTY);
});

test("the new-pool door with drafted accounts creates the pool at once and selects it", async () => {
  const seen = await onPools({ ...LIVE, action: "new", drafted: ["acct-6", "acct-7"] }, async (page) => {
    await page.evaluate("Fixture.settle(120)");
    return { edits: await edits(page), said: await saved(page), items: await items(page), names: await rowNames(page) };
  });
  assert.deepEqual(seen.edits.map((edit) => [edit.kind, edit.pool.name, members(edit)]), [["create_pool", "New pool", ["acct-6", "acct-7"]]]);
  assert.equal(seen.said, "New pool created.");
  assert.deepEqual(seen.items.at(-1).current, "true");
  assert.deepEqual(seen.names, ["Account 6", "Account 7"]);
});

test("the new-pool door with no drafted accounts opens a draft and sends nothing", async () => {
  const seen = await onPools({ ...LIVE, action: "new" }, async (page) => ({ items: await items(page), edits: await edits(page), panel: await page.evaluate("Fixture.count('.pool-add')") }));
  assert.deepEqual(seen.items.at(-1).current, "true");
  assert.equal(seen.items.at(-1).name, "New pool");
  assert.deepEqual(seen.edits, []);
  assert.equal(seen.panel, 1);
});

for (const [how, leave] of [["leaving the field", (page) => pressKey(page, "Tab", 9)], ["Enter", (page) => pressKey(page, "Enter", 13)]]) {
  test(`renaming by ${how} sends one change and says Pool name saved`, async () => {
    const seen = await onPools(LIVE, async (page) => {
      await typeName(page, "Renamed");
      await leave(page);
      return { edits: await edits(page), said: await saved(page), items: await items(page), section: await heading(page) };
    });
    assert.deepEqual(seen.edits.map((edit) => [edit.kind, edit.pool.id, edit.pool.name, members(edit)]), [["replace_pool", "pool-main", "Renamed", ["acct-0", "acct-1", "acct-2"]]]);
    assert.equal(seen.said, "Pool name saved.");
    assert.equal(seen.items[0].name, "Renamed");
    assert.equal(seen.section, "Renamed");
  });
}

for (const [how, text, sentence] of [["empty", "", "Enter a pool name."], ["over 60 characters", "x".repeat(61), "Use a pool name of 1 to 60 characters on one line."]]) {
  test(`a name that is ${how} keeps its sentence on the field and sends nothing`, async () => {
    const seen = await onPools(LIVE, async (page) => {
      await typeName(page, text);
      await pressKey(page, "Enter", 13);
      return { error: await words(page, ".pool-name-edit .field-error"), edits: await edits(page), value: await nameField(page) };
    });
    assert.equal(seen.error, sentence);
    assert.deepEqual(seen.edits, []);
    assert.equal(seen.value, text);
  });
}

test("leaving the name unchanged sends nothing", async () => {
  const sent = await onPools(LIVE, async (page) => {
    await press(page, "#pool-name");
    await pressKey(page, "Tab", 9);
    return edits(page);
  });
  assert.deepEqual(sent, []);
});

test("a rename the engine refuses puts the saved name back and shows its sentence", async () => {
  const seen = await onPools({ ...LIVE, refuseEdit: "pool_name_bounds" }, async (page) => {
    await typeName(page, "Renamed");
    await pressKey(page, "Enter", 13);
    return { alert: await alert(page), value: await nameField(page), section: await heading(page) };
  });
  assert.match(seen.alert, /pool name/i);
  assert.equal(seen.value, "Main pool");
  assert.equal(seen.section, "Main pool");
});

test("deleting a pool in the middle selects the next one in the list", async () => {
  const seen = await onPools(THREE, async (page) => {
    await press(page, ITEM(2));
    await press(page, 'button[aria-label="Delete pool New pool"]');
    await press(page, "dialog[open] button", "Delete pool");
    return { items: await items(page), section: await heading(page) };
  });
  assert.deepEqual(seen.items.map((item) => [item.name, item.current]), [["Main pool", null], ["New pool 2", "true"]]);
  assert.equal(seen.section, "New pool 2");
});

test("deleting a pool projects use unassigns it and selects the next one", async () => {
  const seen = await onPools(LIVE, async (page) => {
    await press(page, 'button[aria-label="Delete pool Main pool"]');
    await press(page, "dialog[open] button", "Unassign and delete");
    return { items: await items(page), edits: await page.evaluate("Fixture.removedPools()"), section: await heading(page) };
  });
  assert.deepEqual(seen.edits, ["pool-main"]);
  assert.deepEqual(seen.items.map((item) => [item.name, item.current]), [["Spare", "true"]]);
  assert.equal(seen.section, "Spare");
});

test("deleting the last pool in the list selects the one before it", async () => {
  const seen = await onPools(THREE, async (page) => {
    await press(page, ITEM(3));
    await press(page, 'button[aria-label="Delete pool New pool 2"]');
    await press(page, "dialog[open] button", "Delete pool");
    return { items: await items(page), section: await heading(page) };
  });
  assert.deepEqual(seen.items.map((item) => [item.name, item.current]), [["Main pool", null], ["New pool", "true"]]);
  assert.equal(seen.section, "New pool");
});
