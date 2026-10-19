import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { promisify } from "node:util";
import { join } from "node:path";

const execute = promisify(execFile);
const writable = process.env.CRAZYAGENT_DESKTOP_TEST_SUITE === "model-access";

async function idle() {
  await browser.waitUntil(async () => await $('[data-testid="connect-owner"]').isEnabled(), { timeout: 15000 });
}

async function click(selector) {
  const element = await $(selector);
  await element.waitForEnabled({ timeout: 15000 });
  await element.click();
  await idle();
}

async function input(field, value) {
  const selector = `[data-testid="model-field-${field}"]`;
  const element = await $(selector);
  await element.waitForEnabled({ timeout: 10000 });
  await element.setValue(value);
  await browser.waitUntil(async () => await $(selector).getValue() === value, { timeout: 10000 });
}

async function snapshot() {
  const result = await execute(process.env.CRAZYAGENT_DESKTOP_TEST_FIXTURE, ["observe-model-access"], { timeout: 10000, maxBuffer: 65536 });
  const actual = JSON.parse(result.stdout);
  assert.equal(actual.collections.length, 3);
  return Object.fromEntries(actual.collections.map((page) => [page.collection, page.items]));
}

async function save() {
  await click('[data-testid="save-model-access"]');
  await browser.waitUntil(async () => !(await $('[data-testid="model-access-form"]').isExisting()) && (await $(".notice-region").getText()).includes("The engine recorded the decision"), { timeout: 10000 });
  assert.equal(await $('[data-testid="model-access-form"]').isExisting(), false);
}

async function remove(id) {
  await click(`[data-model-id="${id}"]`);
  await click('[data-testid="remove-model-access"]');
  assert.equal(await $('[data-testid="model-field-id"]').isEnabled(), false);
  await save();
}

describe("actual native desktop model access", () => {
  it("reads real authority and keeps unsupported authentication explicit", async () => {
    await idle();
    await click("button=Settings");
    await browser.waitUntil(async () => (await $('[data-testid="model-access-authority"]').getText()).startsWith(writable ? "Writable:" : "Read-only:"), { timeout: 10000 });
    assert((await $('[data-testid="model-auth-unavailable"]').getText()).includes("authentication are unavailable"));
    assert.equal(await $('[data-testid="create-model-access"]').isEnabled(), writable);
    assert.deepEqual(await snapshot(), { accounts: [], connections: [], pools: [] });
    await browser.saveScreenshot(join(process.env.CRAZYAGENT_DESKTOP_TEST_EVIDENCE, "model-access-authority.png"));
  });

  if (writable) {
    it("publishes all eight edits through the real owner and retains a form through reconnect", async () => {
      await click('[data-testid="create-model-access"]');
      await input("id", "account");
      await click('[data-testid="connect-owner"]');
      assert.equal(await $('[data-testid="model-field-id"]').getValue(), "account");
      assert.equal(await $('[data-testid="save-model-access"]').isEnabled(), false);
      await click('[data-testid="review-model-access"]');
      await save();
      assert.equal((await snapshot()).accounts[0].account_ref, "account");
      await click('[data-testid="model-tab-connections"]');
      await click('[data-testid="create-model-access"]');
      for (const [field, value] of Object.entries({ id: "connection", provider: "provider", harness: "harness", protocol: "openai", account: "account", url: "https://example.test/v1", model: "author/model", author: "author" })) await input(field, value);
      await $('[data-testid="model-field-auth"]').selectByAttribute("value", "api_key");
      await input("credential", "stored-reference");
      await input("url", "https://user:password@example.test");
      await click('[data-testid="save-model-access"]');
      assert((await $(".notice-region").getText()).includes("no credentials"));
      assert.equal((await snapshot()).connections.length, 0);
      await input("url", "https://example.test/v1");
      await save();
      await click('[data-model-id="connection"]');
      await click('[data-testid="replace-model-access"]');
      assert.equal(await $('[data-testid="model-field-id"]').isEnabled(), false);
      await input("model", "author/replacement");
      await save();
      assert.equal((await snapshot()).connections[0].connection.model.id, "author/replacement");
      await click('[data-testid="model-tab-pools"]');
      await click('[data-testid="create-model-access"]');
      for (const [field, value] of Object.entries({ id: "pool", members: "connection", projects: "project", roles: "builder" })) await input(field, value);
      await save();
      await click('[data-model-id="pool"]');
      await click('[data-testid="replace-model-access"]');
      await input("roles", "reviewer");
      await $('[data-testid="model-field-fallback"]').click();
      await save();
      assert.equal((await snapshot()).pools[0].pool.fallback_allowed, true);
      await browser.saveScreenshot(join(process.env.CRAZYAGENT_DESKTOP_TEST_EVIDENCE, "model-access-pool.png"));
      await click('[data-testid="model-tab-connections"]');
      await click('[data-model-id="connection"]');
      await click('[data-testid="remove-model-access"]');
      await click('[data-testid="save-model-access"]');
      assert((await $(".notice-region").getText()).includes("engine rejected"));
      assert.equal((await snapshot()).connections.length, 1);
      await click('[data-testid="discard-model-access"]');
      await click('[data-testid="model-tab-pools"]');
      await remove("pool");
      await click('[data-testid="model-tab-connections"]');
      await remove("connection");
      await click('[data-testid="model-tab-accounts"]');
      await remove("account");
      assert.deepEqual(await snapshot(), { accounts: [], connections: [], pools: [] });
      await click("button=Approvals");
      await browser.waitUntil(async () => await $("[data-baseline-id]").isExisting(), { timeout: 10000 });
    });
  }
});
