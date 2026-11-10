import assert from "node:assert/strict";
import { dirname, join } from "node:path";
import { DatabaseSync } from "node:sqlite";
import { isDeepStrictEqual } from "node:util";
import { closedPort } from "./e2e-harness.mjs";
import { click, finishWelcome, says, selectPool, settingsTab, shot, shown, texts, toasts, type, until } from "./settings-steps.mjs";
import { accounts, command } from "./trial4-accounts.mjs";

const UI_MS = 15000;
const ENTER = "";
const DAY_MS = 86400000;
const ALPHA = "Alpha";
const BRAVO = "Bravo";
const CHARLIE = "Charlie";
const SIERRA = "Sierra";
const MAIN = "Main pool";
const RENAMED = "Main pool two";
const SPARE = "Spare pool";
const PROJECT = "Pool user";
const DRAFT = "New pool";
const DRAFT_EMPTY = "No accounts yet. This pool is created with its first account.";
const APP = "How every pool picks an account";
const POOL_ORDER = "Account order";
const USAGE = [[ALPHA, 20, 6 * DAY_MS], [BRAVO, 60, DAY_MS], [CHARLIE, 40, 3 * DAY_MS]];
const WHY = {
  soonest: /^Its weekly limit resets soonest, \S.*\.$/u,
  later: /^Its weekly limit resets later, \S.*\.$/u,
  least: "Least weekly use of the accounts ready now.",
  more: "More weekly use than the one above.",
  unranked: "No rank, so it goes after ranked accounts.",
  useLast: "Pinned to Use last, so it goes after accounts that are not.",
  neverUse: "Never use. You pinned it.",
  notSignedIn: "Not signed in. Agents skip it until you sign in again. Sign in",
};
const ORDERS = {
  use: {
    label: "Use it or lose it",
    value: "use_it_or_lose_it",
    hint: "The account whose weekly limit resets soonest goes first, then your rank.",
    rows: [[BRAVO, WHY.soonest], [CHARLIE, WHY.later], [ALPHA, WHY.later]],
  },
  manual: {
    label: "Manual rank",
    value: "manual_rank",
    hint: "Accounts go in the order you rank them.",
    rows: [[ALPHA, WHY.unranked], [BRAVO, WHY.unranked], [CHARLIE, WHY.unranked]],
  },
  balanced: {
    label: "Balanced",
    value: "balanced",
    hint: "The account with the least weekly use goes first.",
    rows: [[ALPHA, WHY.least], [CHARLIE, WHY.more], [BRAVO, WHY.more]],
  },
};
const SIGNED_OUT = [SIERRA, WHY.notSignedIn];
const ids = {};
let generation = 1000000;

/** Says a rank's why line. Input: the rank. Output: the sentence. */
const ranked = (rank) => `You ranked it ${rank}.`;

/** Builds the rows a pool should show: numbered ready accounts, then held ones with no number.
 * Inputs: the ready [name, why] pairs and the held ones. Output: [place, name, why] rows. */
function lineup(ready, held = [SIGNED_OUT]) {
  return [...ready.map(([name, why], at) => [String(at + 1), name, why]), ...held.map(([name, why]) => ["-", name, why])];
}

/** Writes usage rows into the fixture engine's own store, as a poll of each account would. A
 * usage report comes first, because this fixture makes the limits table only on a usage read.
 * Input: [name, weekly percent, ms until the weekly reset] rows. Output: none. */
async function seedUsage(rows) {
  await command("desktop_usage_report", { projectId: null, sinceMs: null, offsetMinutes: 0 });
  const now = Date.now();
  const store = new DatabaseSync(join(dirname(process.env.CRAZYAGENT_DESKTOP_SOCKET), "engine.db"));
  try {
    store.exec("PRAGMA busy_timeout = 5000");
    const insert = store.prepare("INSERT INTO account_limits (account,plan,weekly_percent,weekly_resets_ms,source,observed_ms,checked_ms,weekly_polled_percent,weekly_polled_resets_ms) VALUES (?,'test',?,?,'poll',?,?,?,?)");
    for (const [name, percent, away] of rows) {
      assert.ok(ids[name], `${name} was added first`);
      insert.run(ids[name], percent, now + away, now, now, percent, now + away);
    }
  } finally {
    store.close();
  }
}

/** Adds an OpenAI-compatible URL account through the app's own command. A URL account is never
 * held for sign-in, so it stands in for a key account here with no secret stored. Inputs: the
 * name and the URL. Output: its id. */
async function urlAccount(name, url) {
  const account = await command("desktop_add_account", { draft: { provider: "openai_compatible", method: "url", name, url, model: "author/model" }, key: null });
  assert.equal(account.name, name);
  return account.id;
}

/** Reads the rows of the selected pool as the window shows them. Output: place, name, why line
 * and held mark of each row, in page order. */
function shownRows() {
  return browser.execute(() => [...document.querySelectorAll("main .pool-row")].filter((row) => row.getClientRects().length).map((row) => ({
    place: row.querySelector('.pool-pos [aria-hidden="true"]').textContent,
    name: row.querySelector(".pool-name").textContent,
    why: row.querySelector(".pool-why").textContent,
    held: row.classList.contains("held"),
  })));
}

/** Whether one shown row is the expected one. Inputs: the row and [place, name, why], the why as
 * words or a pattern. Output: a boolean. */
function fits(row, [place, name, why]) {
  return row.place === place && row.name === name && row.held === (place === "-") && (why instanceof RegExp ? why.test(row.why) : row.why === why);
}

/** Waits for the window to show exactly these rows, then reads the engine's own ranking and holds
 * it to the same order. Inputs: the expected rows and the order the engine should be using. */
async function rowsAre(expected, strategy) {
  const words = expected.map(([place, name, why]) => `${place} ${name}: ${why}`).join("; ");
  await until(async () => {
    const rows = await shownRows();
    return rows.length === expected.length && rows.every((row, at) => fits(row, expected[at]));
  }, UI_MS, `the rows did not read ${words}`);
  const found = (await command("desktop_account_ranking", {})).find((row) => row.pool === ids.main);
  assert.ok(found, "the engine ranks the pool");
  assert.deepEqual(found.accounts.map((item) => item.connection), expected.map(([, name]) => ids[name]));
  assert.deepEqual(found.accounts.map((item) => item.standing.kind === "next"), expected.map(([place]) => place !== "-"));
  assert.equal(found.strategy, strategy);
}

/** Names one option of a choice group. Inputs: the group's label and the option's words. Output:
 * its selector. */
function option(group, words) {
  return `//*[@role="radiogroup"][@aria-label=${JSON.stringify(group)}]//button[normalize-space(.)=${JSON.stringify(words)}]`;
}

/** Reads what a choice group has picked. Input: the group's label. Output: the option's words. */
function picked(group) {
  return browser.execute((group) => [...document.querySelectorAll('[role="radiogroup"]')].find((box) => box.getAttribute("aria-label") === group)
    ?.querySelector('[aria-checked="true"]')?.textContent ?? null, group);
}

/** Waits for the line under Account order to start with these words. Input: the words. */
async function orderSaid(words) {
  await until(async () => (await texts("main .pool-order p"))[0]?.startsWith(words) === true, UI_MS, `the order line did not start ${words}`);
}

/** Waits for the left list to name exactly these pools, in any order. Input: the names. */
async function poolsListed(names) {
  const wanted = JSON.stringify([...names].sort());
  await until(async () => JSON.stringify((await texts("main .pool-item-label")).sort()) === wanted, UI_MS, `the left list did not name exactly ${wanted}`);
}

/** Waits for the left list to mark this pool selected. Input: its name. */
async function selectedIs(name) {
  await until(() => says("main .pool-item.on .pool-item-label", name), UI_MS, `${name} was not selected in the left list`);
}

/** Reads one pool as the engine holds it. Input: its name. Output: the pool; fails unless there
 * is exactly one of that name. */
async function enginePool(name) {
  const found = (await command("desktop_pool_summaries", {})).filter((pool) => pool.name === name);
  assert.equal(found.length, 1, `the engine holds one pool named ${name}`);
  return found[0];
}

/** Reads one account's plan from the engine. Input: its name. Output: the plan. */
async function planOf(name) {
  const found = (await command("desktop_account_schedule", {})).accounts.find((row) => row.connection === ids[name]);
  return found?.plan ?? { rank: null, reserve_percent: 0, pin: null };
}

/** Reads the plan words on one row. Input: the account's name. Output: the words. */
function planWords(name) {
  return browser.execute((name) => [...document.querySelectorAll("main .pool-row")].find((row) => row.querySelector(".pool-name").textContent === name)
    ?.querySelector(".pool-plan").textContent ?? "", name);
}

/** Waits for one row's plan words to end this way. Inputs: the account's name and the words. */
async function planReads(name, words) {
  await until(async () => (await planWords(name)).endsWith(words), UI_MS, `the row of ${name} did not end ${words}`);
}

/** Opens one row's Schedule panel when it is not open. Input: the account's name. */
async function openSchedule(name) {
  const field = `#pool-rank-${ids[name]}`;
  if (!(await shown(field))) await click(`[data-control="schedule:${ids[name]}"]`);
  await until(() => shown(field), UI_MS, `the Schedule panel of ${name} did not open`);
}

/** Puts a value in a field and presses Enter on it, as saving a plan number or a pool name takes.
 * Inputs: the field's selector and the value, empty to clear it. */
async function commit(selector, value) {
  if (value === "") {
    const field = await $(selector);
    await field.clearValue();
    await until(async () => await field.getValue() === "", UI_MS, `${selector} did not clear`);
  } else {
    await type(selector, value);
  }
  await browser.keys(ENTER);
}

/** Reads where the keyboard is. Output: the row control holding it, its words and its checked mark. */
function focused() {
  return browser.execute(() => {
    const at = document.activeElement;
    return { control: at?.closest("[data-control]")?.dataset.control ?? null, words: at?.textContent ?? "", checked: at?.getAttribute("aria-checked") ?? null };
  });
}

/** Waits for exactly this toast. Input: its words. */
async function toast(words) {
  await until(async () => JSON.stringify(await toasts()) === JSON.stringify([words]), UI_MS, `the toast did not say exactly ${words}`);
}

/** Makes a project and gives it a pool through the app's own commands. Input: the pool's id. */
async function projectOn(pool) {
  const receipt = await command("desktop_create_project", { spec: { name: PROJECT, repositories: [join(process.env.HOME, "pool-user-repo")] }, generation: ++generation });
  assert.equal(receipt.outcome, "applied", `the project is made: ${JSON.stringify(receipt)}`);
  const id = receipt.project_id;
  assert.equal((await command("desktop_open_project", { id })).project_id, id, "the new project opens");
  const page = await command("desktop_model_access_page", { collection: "pools", next: false });
  assert.equal(page.writable, true, "the test engine's settings are writable");
  const assigned = await command("desktop_edit_model_access", { ticket: page.ticket, edit: { kind: "assign_project_pool", project: id, pool }, generation: ++generation });
  assert.equal(assigned.outcome, "applied", `the project gets the pool: ${JSON.stringify(assigned)}`);
}

describe("Settings, Pools as one screen on the real desktop app and a writable engine", () => {
  it("starts with three URL accounts, a subscription with no sign-in, usage rows and no pool", async () => {
    await finishWelcome("Pools Owner");
    await click("dialog[open] button[aria-label='Close']");
    await until(async () => !(await shown("dialog[open]")), UI_MS, "the Add account dialog did not close");
    const url = `http://127.0.0.1:${await closedPort()}/v1`;
    for (const name of [ALPHA, BRAVO, CHARLIE]) ids[name] = await urlAccount(name, url);
    const signedOut = await command("desktop_add_account", { draft: { provider: "claude", method: "subscription", name: SIERRA }, key: null });
    assert.notEqual(signedOut.status, "signed_in", "a new subscription is not signed in");
    ids[SIERRA] = signedOut.id;
    await seedUsage(USAGE);
    assert.deepEqual((await accounts()).map((account) => account.name).sort(), [ALPHA, BRAVO, CHARLIE, SIERRA]);
    await settingsTab("Pools");
    await until(() => says("main .empty-state h2", "No pools yet"), UI_MS, "Pools did not open empty");
  });

  it("New pool is selected in the left list at once, with the add panel open and nothing sent", async () => {
    await click("button=New pool");
    await selectedIs(DRAFT);
    assert.equal(await (await $("#pool-name")).getValue(), DRAFT);
    assert.equal(await shown(".pool-add"), true, "the add panel is open");
    await until(() => says("main .pool-empty", DRAFT_EMPTY), UI_MS, "the draft did not say it is created with its first account");
    assert.deepEqual(await command("desktop_pool_summaries", {}), [], "a draft is not in the engine");
    await shot("pools-draft");
  });

  it("the first Add creates the pool under the name typed", async () => {
    await type("#pool-name", MAIN);
    await click(`[data-control="add:${ids[ALPHA]}"]`);
    await until(() => says("main .pool-saved", `${MAIN} created.`), UI_MS, "the first Add did not say the pool was created");
    await poolsListed([MAIN]);
    await selectedIs(MAIN);
    const pool = await enginePool(MAIN);
    ids.main = pool.id;
    assert.deepEqual(pool.members.map((member) => member.id), [ids[ALPHA]]);
    await rowsAre(lineup([[ALPHA, WHY.soonest]], []), ORDERS.use.value);
  });

  it("Add puts each account at its calculated place", async () => {
    await click(`[data-control="add:${ids[BRAVO]}"]`);
    await rowsAre(lineup([[BRAVO, WHY.soonest], [ALPHA, WHY.later]], []), ORDERS.use.value);
    await click(`[data-control="add:${ids[CHARLIE]}"]`);
    await rowsAre(lineup(ORDERS.use.rows, []), ORDERS.use.value);
    await click(`[data-control="add:${ids[SIERRA]}"]`);
    await rowsAre(lineup(ORDERS.use.rows), ORDERS.use.value);
    assert.deepEqual((await enginePool(MAIN)).members.map((member) => member.id), [ids[ALPHA], ids[BRAVO], ids[CHARLIE], ids[SIERRA]]);
    await click("button=Done");
    await until(async () => !(await shown(".pool-add")), UI_MS, "Done did not close the add panel");
    await shot("pools-order");
  });

  it("an account that is not signed in is held with no number and links to its sign-in", async () => {
    assert.deepEqual(await texts("main .pool-row.held .pool-pos .visually-hidden"), ["Held"]);
    const href = await browser.execute((label) => document.querySelector(`a[aria-label="${label}"]`)?.getAttribute("href") ?? null, `Sign in ${SIERRA}`);
    assert.equal(href, `#/settings/accounts?action=sign_in&account=${encodeURIComponent(ids[SIERRA])}`);
    const found = (await command("desktop_account_ranking", {})).find((row) => row.pool === ids.main);
    assert.deepEqual(found.accounts.at(-1), { connection: ids[SIERRA], standing: { kind: "not_signed_in" } });
  });

  for (const key of ["balanced", "manual", "use"]) {
    const order = ORDERS[key];
    it(`the default for every pool, ${order.label}, orders a pool on Default`, async () => {
      await click(option(APP, order.label));
      await rowsAre(lineup(order.rows), order.value);
      await until(() => says("main .pool-app-hint", order.hint), UI_MS, `the hint did not follow ${order.label}`);
      await until(async () => await picked(POOL_ORDER) === `Default (${order.label})`, UI_MS, `Default did not name ${order.label}`);
      await orderSaid(`Order comes from ${order.label}, the default for every pool`);
      const saved = await command("desktop_account_schedule", {});
      assert.equal(saved.strategy, order.value);
      assert.deepEqual(saved.pools, []);
    });
  }

  for (const [key, app] of [["balanced", "use"], ["manual", "use"], ["use", "balanced"]]) {
    const order = ORDERS[key];
    const under = ORDERS[app];
    it(`a pool set to ${order.label} follows it under the default ${under.label}`, async () => {
      if (await picked(APP) !== under.label) await click(option(APP, under.label));
      await until(() => shown(option(POOL_ORDER, `Default (${under.label})`)), UI_MS, `Default did not name ${under.label}`);
      await click(option(POOL_ORDER, order.label));
      await rowsAre(lineup(order.rows), order.value);
      await orderSaid(`Order comes from ${order.label}, this pool`);
      const saved = await command("desktop_account_schedule", {});
      assert.equal(saved.strategy, under.value);
      assert.deepEqual(saved.pools, [{ id: ids.main, strategy: order.value }]);
    });
  }

  it("a pool back on Default follows the default for every pool again", async () => {
    await click(option(POOL_ORDER, "Default (Balanced)"));
    await rowsAre(lineup(ORDERS.balanced.rows), ORDERS.balanced.value);
    await orderSaid("Order comes from Balanced, the default for every pool");
    assert.deepEqual((await command("desktop_account_schedule", {})).pools, []);
  });

  it("Use last moves the row behind the others", async () => {
    await click(option(`Pin for ${ALPHA}`, "Use last"));
    await rowsAre(lineup([[CHARLIE, WHY.least], [BRAVO, WHY.more], [ALPHA, WHY.useLast]]), ORDERS.balanced.value);
    assert.equal((await planOf(ALPHA)).pin, "use_last");
  });

  it("None brings the row back and the keyboard stays on the pin it moved with", async () => {
    await click(option(`Pin for ${ALPHA}`, "None"));
    await rowsAre(lineup(ORDERS.balanced.rows), ORDERS.balanced.value);
    assert.equal((await planOf(ALPHA)).pin, null);
    const expected = { control: `pin:${ids[ALPHA]}`, words: "None", checked: "true" };
    await until(async () => isDeepStrictEqual(await focused(), expected), UI_MS, `the keyboard did not stay on the pin: ${JSON.stringify(expected)}`);
  });

  it("Never use holds the row with no number, and None returns it", async () => {
    await click(option(`Pin for ${ALPHA}`, "Never use"));
    await rowsAre(lineup([[CHARLIE, WHY.least], [BRAVO, WHY.more]], [[ALPHA, WHY.neverUse], SIGNED_OUT]), ORDERS.balanced.value);
    assert.equal((await planOf(ALPHA)).pin, "never_use");
    await shot("pools-never-use");
    await click(option(`Pin for ${ALPHA}`, "None"));
    await rowsAre(lineup(ORDERS.balanced.rows), ORDERS.balanced.value);
  });

  it("under Manual rank a rank moves the row, and clearing it moves it back", async () => {
    await click(option(POOL_ORDER, ORDERS.manual.label));
    await rowsAre(lineup(ORDERS.manual.rows), ORDERS.manual.value);
    await openSchedule(CHARLIE);
    await commit(`#pool-rank-${ids[CHARLIE]}`, "1");
    await rowsAre(lineup([[CHARLIE, ranked(1)], [ALPHA, WHY.unranked], [BRAVO, WHY.unranked]]), ORDERS.manual.value);
    assert.equal((await planOf(CHARLIE)).rank, 1);
    await commit(`#pool-rank-${ids[CHARLIE]}`, "");
    await rowsAre(lineup(ORDERS.manual.rows), ORDERS.manual.value);
    assert.equal((await planOf(CHARLIE)).rank, null);
  });

  it("under Balanced a rank is saved and the order holds", async () => {
    await click(option(POOL_ORDER, "Default (Balanced)"));
    await rowsAre(lineup(ORDERS.balanced.rows), ORDERS.balanced.value);
    await openSchedule(BRAVO);
    await commit(`#pool-rank-${ids[BRAVO]}`, "1");
    await planReads(BRAVO, "Rank 1, used to 100%");
    await rowsAre(lineup(ORDERS.balanced.rows), ORDERS.balanced.value);
    assert.equal((await planOf(BRAVO)).rank, 1);
    await commit(`#pool-rank-${ids[BRAVO]}`, "");
    await planReads(BRAVO, "No rank, used to 100%");
    assert.equal((await planOf(BRAVO)).rank, null);
  });

  it("a reserve the account has reached holds its row, and clearing it brings the row back", async () => {
    await openSchedule(BRAVO);
    await commit(`#pool-reserve-${ids[BRAVO]}`, "50");
    await rowsAre(lineup([[ALPHA, WHY.least], [CHARLIE, WHY.more]], [[BRAVO, "Reserve reached (50%)"], SIGNED_OUT]), ORDERS.balanced.value);
    assert.equal((await planOf(BRAVO)).reserve_percent, 50);
    await shot("pools-reserve");
    await commit(`#pool-reserve-${ids[BRAVO]}`, "");
    await rowsAre(lineup(ORDERS.balanced.rows), ORDERS.balanced.value);
    assert.equal((await planOf(BRAVO)).reserve_percent, 0);
  });

  it("Remove takes the account out of the pool and keeps it in the list", async () => {
    await click(`[data-control="remove:${ids[CHARLIE]}"]`);
    await rowsAre(lineup([[ALPHA, WHY.least], [BRAVO, WHY.more]]), ORDERS.balanced.value);
    assert.deepEqual((await enginePool(MAIN)).members.map((member) => member.id), [ids[ALPHA], ids[BRAVO], ids[SIERRA]]);
    assert.equal((await accounts()).length, 4, "the account stays");
  });

  it("Add puts a removed account back at its calculated place", async () => {
    await click('[data-control="add-account"]');
    await click(`[data-control="add:${ids[CHARLIE]}"]`);
    await rowsAre(lineup(ORDERS.balanced.rows), ORDERS.balanced.value);
    assert.deepEqual((await enginePool(MAIN)).members.map((member) => member.id), [ids[ALPHA], ids[BRAVO], ids[SIERRA], ids[CHARLIE]]);
    await click("button=Done");
  });

  it("leaving a New pool draft with no account drops it, with no question and nothing sent", async () => {
    await click("button=New pool");
    await selectedIs(DRAFT);
    await poolsListed([MAIN, DRAFT]);
    await selectPool(MAIN);
    await poolsListed([MAIN]);
    await selectedIs(MAIN);
    assert.equal(await shown("dialog[open]"), false, "leaving asks nothing");
    assert.deepEqual((await command("desktop_pool_summaries", {})).map((pool) => pool.id), [ids.main]);
  });

  it("the Sign in link lands on that account's sign-in", async () => {
    await click(`a[aria-label="Sign in ${SIERRA}"]`);
    await until(async () => /^#\/settings\/accounts/u.test(await browser.execute(() => location.hash)) && await says("dialog[open] h2", SIERRA), UI_MS,
      "the link did not open the account's details");
    await until(() => shown("button=Open Claude sign-in"), UI_MS, "the details did not open on the sign-in");
    await shot("pools-sign-in-link");
    await click("dialog[open] button[aria-label='Close']");
    await until(async () => !(await shown("dialog[open]")), UI_MS, "the details did not close");
    await settingsTab("Pools");
    await rowsAre(lineup(ORDERS.balanced.rows), ORDERS.balanced.value);
  });

  it("renames the pool from its name field and the engine keeps its id", async () => {
    await commit("#pool-name", RENAMED);
    await poolsListed([RENAMED]);
    const pool = await enginePool(RENAMED);
    assert.equal(pool.id, ids.main);
  });

  it("deletes a pool no project uses, keeping its accounts", async () => {
    await click("button=New pool");
    await type("#pool-name", SPARE);
    await click(`[data-control="add:${ids[ALPHA]}"]`);
    await poolsListed([RENAMED, SPARE]);
    await selectedIs(SPARE);
    await until(async () => (await command("desktop_pool_summaries", {})).length === 2, UI_MS, "the engine did not create the second pool");
    await click("button=Done");
    await click(`button[aria-label=${JSON.stringify(`Delete pool ${SPARE}`)}]`);
    await until(() => says("dialog[open] p", "Its accounts stay in your list. Only the pool goes away."), UI_MS, "the delete question did not open");
    await click('//dialog[@open]//button[normalize-space(.)="Delete pool"]');
    await toast(`${SPARE} deleted.`);
    await poolsListed([RENAMED]);
    await selectedIs(RENAMED);
    assert.deepEqual((await command("desktop_pool_summaries", {})).map((pool) => pool.id), [ids.main]);
    assert.equal((await accounts()).length, 4, "the accounts stay");
  });

  it("deleting a pool a project uses names the project and leaves it with no pool", async () => {
    await projectOn(ids.main);
    await settingsTab("Accounts");
    await settingsTab("Pools");
    await until(async () => (await texts("main .pool-item-about"))[0]?.endsWith("Used by 1 project.") === true, UI_MS, "the left list did not say the pool is used");
    await click(`button[aria-label=${JSON.stringify(`Delete pool ${RENAMED}`)}]`);
    await until(() => says("dialog[open] p", `Used by ${PROJECT}. It will have no pool until you pick one. Its accounts stay in your list.`), UI_MS, "the delete question did not name the project");
    await shot("pools-delete-used");
    await click('//dialog[@open]//button[normalize-space(.)="Unassign and delete"]');
    await toast(`${RENAMED} deleted. Its projects have no pool now. Pick a pool to start in each project's Settings.`);
    await until(() => says("main .empty-state h2", "No pools yet"), UI_MS, "the pool was not deleted");
    assert.deepEqual(await command("desktop_pool_summaries", {}), []);
    assert.equal((await accounts()).length, 4, "the accounts stay");
  });
});
