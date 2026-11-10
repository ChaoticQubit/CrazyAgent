import assert from "node:assert/strict";
import { closedPort } from "./e2e-harness.mjs";
import { click, finishWelcome, menu, observeModelAccess, pick, says, selectPool, settingsTab, shot, shown, texts, toasts, type, until } from "./settings-steps.mjs";

const LOCAL = "Local closed";
const RENAMED = "Local renamed";
const COMPATIBLE = "Compatible closed";
const POOL = "Native pool";
const POOL_RENAMED = "Native pool two";
const UNREACHABLE = "Saved. Can't reach this URL. Check the address and that the server is running.";
const ids = {};
const same = (left, right) => JSON.stringify(left) === JSON.stringify(right);

async function listed(names, count) {
  await until(async () => same(await texts("main .account-row .cell .ellipsis"), names) && same(await texts("main .count"), [count]), 15000,
    `the account list did not show exactly ${names.join(", ")}`);
}

async function pools(rows) {
  await until(async () => same([await texts("main .pool-item-label"), await texts("main .pool-item-count")], [rows.map(([name]) => name), rows.map(([, members]) => members)]),
    30000, `the pool list did not show exactly ${JSON.stringify(rows)}`);
}

async function addToPool(name) {
  await click(`button[aria-label=${JSON.stringify(`Add ${name}`)}]`);
}

async function toast(message) {
  await until(async () => same(await toasts(), [message]), 15000, `the toast did not say exactly ${message}`);
}

function saved(engine, id) {
  const account = engine.registry.find((item) => item.id === id);
  assert.ok(account, `the engine keeps ${id}`);
  return account;
}

async function addUrlAccount(tile, name, url, model) {
  await pick("tile", tile);
  await until(() => shown("#account-url"), 15000, `${tile} form did not open`);
  await type("#account-name", name);
  await type("#account-url", url);
  await type("#account-model", model);
  await click("button=Save");
  await until(() => says("dialog[open] .success p:last-child", UNREACHABLE), 30000, `${name} was not saved and checked`);
  assert.deepEqual(await texts("dialog[open] .success strong"), [name]);
}

describe("Settings, Accounts and Pools on the real desktop app and a writable engine", () => {
  it("first run ends on Add account with nothing saved in the engine", async () => {
    await finishWelcome("Model Access Owner");
    const engine = await observeModelAccess();
    assert.equal(engine.writable, true);
    assert.deepEqual([engine.registry, engine.accounts, engine.connections, engine.pools], [[], [], [], []]);
    await shot("add-account-open");
  });

  it("adds a local model account that nothing answers yet", async () => {
    const url = `http://127.0.0.1:${await closedPort()}/v1`;
    await addUrlAccount("Local models (Ollama)", LOCAL, url, "qwen-native-test");
    const engine = await observeModelAccess();
    assert.equal(engine.registry.length, 1);
    const [local] = engine.registry;
    assert.match(local.id, /^local-account-[0-9a-f]{20}$/);
    assert.deepEqual([local.name, local.provider, local.url, local.model, local.status],
      [LOCAL, "local", url, "qwen-native-test", "unreachable"]);
    ids.local = local.id;
  });

  it("adds an OpenAI-compatible URL account without a key", async () => {
    await click("button=Add another account");
    const url = `http://127.0.0.1:${await closedPort()}/v1`;
    await addUrlAccount("Any OpenAI-compatible URL", COMPATIBLE, url, "author/model");
    const engine = await observeModelAccess();
    assert.equal(engine.registry.length, 2);
    const compatible = engine.registry.find((account) => account.id !== ids.local);
    assert.match(compatible.id, /^url-account-[0-9a-f]{20}$/);
    assert.deepEqual([compatible.name, compatible.provider, compatible.url, compatible.model, compatible.status],
      [COMPATIBLE, "openai_compatible", url, "author/model", "unreachable"]);
    saved(engine, ids.local);
    ids.compatible = compatible.id;
    await click("button=Done");
    await listed([LOCAL, COMPATIBLE], "2 accounts");
    await shot("two-accounts");
  });

  it("renames an account from its details and the engine keeps the connection", async () => {
    await click(`text=${LOCAL}`);
    await until(() => shown("#drawer-account-name"), 15000, "the account details did not open");
    await type("#drawer-account-name", RENAMED);
    await click("button=Rename");
    await until(() => says("dialog[open] h2", RENAMED), 15000, "the details did not show the new name");
    await click("dialog[open] button[aria-label='Close']");
    await settingsTab("Pools");
    await settingsTab("Accounts");
    await listed([RENAMED, COMPATIBLE], "2 accounts");
    const engine = await observeModelAccess();
    assert.deepEqual(engine.registry.map((account) => account.id).sort(), [ids.local, ids.compatible].sort());
    assert.equal(saved(engine, ids.local).name, RENAMED);
  });

  it("creates a pool holding both accounts", async () => {
    await settingsTab("Pools");
    await click("button=New pool");
    await type("#pool-name", POOL);
    await addToPool(RENAMED);
    await pools([[POOL, "1 of 15"]]);
    await addToPool(COMPATIBLE);
    await pools([[POOL, "2 of 15"]]);
    await shot("pool-saved");
    const [pool, ...others] = (await observeModelAccess()).pools;
    assert.deepEqual(others, []);
    assert.equal(pool.name, POOL);
    assert.deepEqual(pool.members, [ids.local, ids.compatible]);
    assert.deepEqual(pool.projects, []);
    assert.equal(pool.fallback_allowed, false);
    ids.pool = pool.id;
  });

  it("edits the pool: renames it and takes one account out", async () => {
    await selectPool(POOL);
    await type("#pool-name", POOL_RENAMED);
    await browser.keys("\uE007");
    await pools([[POOL_RENAMED, "2 of 15"]]);
    await click(`button[aria-label=${JSON.stringify(`Remove ${COMPATIBLE}`)}]`);
    await pools([[POOL_RENAMED, "1 of 15"]]);
    const [pool] = (await observeModelAccess()).pools;
    assert.equal(pool.id, ids.pool);
    assert.equal(pool.name, POOL_RENAMED);
    assert.deepEqual(pool.members, [ids.local]);
  });

  it("deleting the only account of a pool deletes that pool too, as the question says", async () => {
    await settingsTab("Accounts");
    await menu(`More for ${RENAMED}`, "Delete");
    await until(() => says("dialog[open] h2", `Delete ${RENAMED}?`), 15000, "the delete question did not open");
    assert.deepEqual(await texts("dialog[open] p .stack-small > span"), [
      `${RENAMED} and its saved sign-in or key are removed from CrazyAgent. A running sign-in is cancelled first.`,
      "An agent using this account keeps running. Its sign-in goes after that agent stops and the app restarts.",
      `It leaves pool ${POOL_RENAMED}. ${POOL_RENAMED} has no other account, so it is deleted too and its projects have no pool.`,
    ]);
    await click("button=Remove from pools and delete");
    await toast(`Deleted ${RENAMED}.`);
    const engine = await observeModelAccess();
    assert.deepEqual(engine.registry.map((account) => account.id), [ids.compatible]);
    assert.deepEqual(engine.pools, []);
    await settingsTab("Pools");
    await until(() => says("main .empty-state h2", "No pools yet"), 15000, "the emptied pool still showed");
  });

  it("deletes a pool from its row, keeping its account, then the last account, leaving the engine empty", async () => {
    await click("button=New pool");
    await type("#pool-name", POOL);
    await addToPool(COMPATIBLE);
    await pools([[POOL, "1 of 15"]]);
    await selectPool(POOL);
    await click(`button[aria-label=${JSON.stringify(`Delete pool ${POOL}`)}]`);
    await until(() => says("dialog[open] p", "Its accounts stay in your list. Only the pool goes away."), 15000, "the pool delete question did not open");
    await click('//dialog[@open]//button[normalize-space(.)="Delete pool"]');
    await toast(`${POOL} deleted.`);
    await until(() => says("main .empty-state h2", "No pools yet"), 15000, "the pool was not deleted");
    const kept = await observeModelAccess();
    assert.deepEqual([kept.registry.map((account) => account.id), kept.pools], [[ids.compatible], []]);
    await settingsTab("Accounts");
    await menu(`More for ${COMPATIBLE}`, "Delete");
    await until(() => says("dialog[open] h2", `Delete ${COMPATIBLE}?`), 15000, "the delete question did not open");
    await click("button=Delete account");
    await toast(`Deleted ${COMPATIBLE}.`);
    await until(() => says("main .empty-state h2", "No accounts yet"), 15000, "the last account was not deleted");
    const engine = await observeModelAccess();
    assert.deepEqual([engine.registry, engine.connections, engine.pools], [[], [], []]);
    await shot("empty-again");
  });
});
