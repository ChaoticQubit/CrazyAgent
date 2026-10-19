import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { promisify } from "node:util";
import { join } from "node:path";
import { basename, dirname, isAbsolute } from "node:path";
import { closeSync, constants, fstatSync, lstatSync, openSync, readSync, realpathSync, writeFileSync } from "node:fs";
import { createHash } from "node:crypto";

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

function quotaSettings() {
  const socket = process.env.CRAZYAGENT_DESKTOP_SOCKET;
  const evidence = process.env.CRAZYAGENT_DESKTOP_TEST_EVIDENCE;
  if (!socket || !isAbsolute(socket) || basename(socket) !== "e.sock" || !evidence || !isAbsolute(evidence)) throw new Error("Declared absolute quota fixture socket and evidence paths required.");
  const root = dirname(socket);
  const canonical = realpathSync(root);
  if (dirname(canonical) !== "/private/tmp" || !/^cad-[A-Za-z0-9_-]+$/.test(basename(canonical))) throw new Error("Declared private cad fixture root required.");
  const uid = BigInt(process.getuid());
  const directories = [root, evidence].map((path) => {
    const info = lstatSync(path, { bigint: true });
    if (!info.isDirectory() || info.uid !== uid || (info.mode & 0o077n) !== 0n) throw new Error("Quota fixture and evidence directories must be owned and private.");
    return { path, info };
  });
  const path = join(root, "g");
  const descriptor = openSync(path, constants.O_RDONLY | constants.O_NOFOLLOW);
  try {
    const before = fstatSync(descriptor, { bigint: true });
    if (!before.isFile() || before.uid !== uid || (before.mode & 0o077n) !== 0n || before.nlink !== 1n || before.size > 1024n * 1024n) throw new Error("Quota settings must be one owned private regular file within 1MiB.");
    const bytes = Buffer.alloc(Number(before.size) + 1);
    let used = 0;
    while (used < bytes.length) {
      const count = readSync(descriptor, bytes, used, bytes.length - used, null);
      if (count === 0) break;
      used += count;
    }
    const after = fstatSync(descriptor, { bigint: true });
    const linked = lstatSync(path, { bigint: true });
    if (BigInt(used) !== before.size || after.size !== before.size || after.mtimeNs !== before.mtimeNs || after.ctimeNs !== before.ctimeNs || !linked.isFile() || linked.dev !== before.dev || linked.ino !== before.ino || linked.size !== before.size || linked.mtimeNs !== before.mtimeNs || linked.ctimeNs !== before.ctimeNs) throw new Error("Actual quota settings changed during bounded readback.");
    for (const directory of directories) {
      const current = lstatSync(directory.path, { bigint: true });
      if (!current.isDirectory() || current.dev !== directory.info.dev || current.ino !== directory.info.ino || current.uid !== uid || (current.mode & 0o077n) !== 0n) throw new Error("Quota readback directory identity changed.");
    }
    return { path, evidence, bytes: bytes.subarray(0, used) };
  } finally {
    closeSync(descriptor);
  }
}

async function quotaReadback(phase, policy, previousRevision) {
  if (!["default", "draft", "strict", "bounded"].includes(phase) || !["bounded_attempt", "strict"].includes(policy)) throw new Error("Closed quota readback phase and policy required.");
  const result = await execute(process.env.CRAZYAGENT_DESKTOP_TEST_FIXTURE, ["observe-model-access"], { timeout: 10000, maxBuffer: 65536 });
  const settings = quotaSettings();
  const options = { flag: "wx", mode: 0o600 };
  writeFileSync(join(settings.evidence, `quota-${phase}-owner.json`), result.stdout, options);
  writeFileSync(join(settings.evidence, `quota-${phase}-owner.stderr`), result.stderr, options);
  writeFileSync(join(settings.evidence, `quota-${phase}-g.toml`), settings.bytes, options);
  assert.equal(result.stderr, "", "actual quota observer must succeed without diagnostics");
  assert(Buffer.byteLength(result.stdout) <= 4096, "actual fixture JSON emission bound");
  const actual = JSON.parse(result.stdout);
  assert.deepEqual(actual.collections.map((page) => page.collection), ["accounts", "connections", "pools"]);
  for (const page of actual.collections) {
    assert.equal(page.writable, true, "independent owner must report actual writable sources");
    assert.equal(typeof page.revision, "string");
    assert.match(page.revision, /^(0|[1-9][0-9]*)$/);
    assert(Array.isArray(page.items), "actual owner collection required");
  }
  const page = actual.collections[2];
  if (previousRevision !== undefined) assert(BigInt(page.revision) >= BigInt(previousRevision), "actual owner revision must not regress");
  const pool = { id: "pool", members: ["connection"], projects: ["project"], roles: ["builder"], fallback_allowed: false };
  assert.deepEqual(page.items, [{ kind: "pool", pool: policy === "strict" ? { ...pool, provider_quota_policy: "strict" } : pool }]);
  assert.equal(Object.hasOwn(page.items[0].pool, "provider_quota_policy"), policy === "strict", "bounded policy must be omitted on actual owner readback");
  const text = settings.bytes.toString("utf8");
  const tables = text.split(/^[ \t]*\[\[model_access\.pools\]\][ \t]*\r?$/m);
  assert.equal(tables.length, 2, "actual settings must contain the one persisted pool table");
  const persisted = tables[1].split(/^[ \t]*\[/m)[0];
  assert.match(persisted, /^\s*id\s*=\s*(?:"pool"|'pool')\s*$/m);
  assert.match(persisted, /^\s*members\s*=\s*\[\s*(?:"connection"|'connection')\s*,?\s*\]\s*$/m);
  assert.match(persisted, /^\s*projects\s*=\s*\[\s*(?:"project"|'project')\s*,?\s*\]\s*$/m);
  assert.match(persisted, /^\s*roles\s*=\s*\[\s*(?:"builder"|'builder')\s*,?\s*\]\s*$/m);
  assert.match(persisted, /^\s*fallback_allowed\s*=\s*false\s*$/m);
  if (policy === "strict") {
    assert.equal((text.match(/^\s*provider_quota_policy\s*=/gm) ?? []).length, 1);
    assert.match(persisted, /^\s*provider_quota_policy\s*=\s*(?:"strict"|'strict')\s*$/m, "strict must persist in actual owner-produced settings");
  } else {
    assert.doesNotMatch(text, /^\s*provider_quota_policy\s*=/m, "bounded default must be omitted from actual settings");
  }
  const basis = $('[data-testid="model-access-basis"]');
  const proof = {
    phase,
    owner_revision: page.revision,
    owner_writable: page.writable,
    settings_path: settings.path,
    settings_bytes: settings.bytes.length,
    settings_sha256: createHash("sha256").update(settings.bytes).digest("hex"),
    ui_authority: await $('[data-testid="model-access-authority"]').getText(),
    ui_basis: await basis.isExisting() ? await basis.getText() : null,
  };
  const encoded = JSON.stringify(proof, null, 2);
  assert(Buffer.byteLength(encoded) <= 1024 * 1024, "quota evidence byte bound");
  writeFileSync(join(settings.evidence, `quota-${phase}-proof.json`), encoded, options);
  return proof;
}

async function quotaForm(policy) {
  const field = await $('[data-testid="model-field-providerQuotaPolicy"]');
  await field.waitForEnabled({ timeout: 10000 });
  assert.equal(await field.getTagName(), "select");
  assert.equal(await field.getValue(), policy);
  const options = [];
  for (const option of await field.$$("option")) options.push([await option.getAttribute("value"), await option.getText()]);
  assert.deepEqual(options, [["bounded_attempt", "Bounded attempt (default)"], ["strict", "Strict quota"]]);
  assert.equal(await field.getAttribute("aria-describedby"), "model-quota-policy-help");
  const help = await $("#model-quota-policy-help").getText();
  assert.match(help, /Strict quota blocks/);
  assert.match(help, /numeric quota is currently unavailable/);
  assert.match(help, /genuine fresh readiness and independent engine admission/);
  return field;
}

async function quotaFlow() {
  const initial = await quotaReadback("default", "bounded_attempt");
  await click('[data-model-id="pool"]');
  await click('[data-testid="replace-model-access"]');
  await (await quotaForm("bounded_attempt")).click();
  await browser.keys("ArrowDown");
  await browser.keys("Enter");
  await browser.waitUntil(async () => await $('[data-testid="model-field-providerQuotaPolicy"]').getValue() === "strict", { timeout: 10000 });
  await click('[data-testid="connect-owner"]');
  await quotaForm("strict");
  assert.equal(await $('[data-testid="save-model-access"]').isEnabled(), false);
  const draft = await quotaReadback("draft", "bounded_attempt", initial.owner_revision);
  assert.equal(draft.settings_sha256, initial.settings_sha256, "unsaved strict draft must not publish settings");
  await click('[data-testid="review-model-access"]');
  await quotaForm("strict");
  assert.equal(await $('[data-testid="save-model-access"]').isEnabled(), true);
  await save();
  const strict = await quotaReadback("strict", "strict", draft.owner_revision);
  assert.notEqual(strict.settings_sha256, initial.settings_sha256, "explicit strict policy must change actual settings");
  await click('[data-model-id="pool"]');
  await click('[data-testid="replace-model-access"]');
  await (await quotaForm("strict")).click();
  await browser.keys("ArrowUp");
  await browser.keys("Enter");
  await browser.waitUntil(async () => await $('[data-testid="model-field-providerQuotaPolicy"]').getValue() === "bounded_attempt", { timeout: 10000 });
  await save();
  await quotaReadback("bounded", "bounded_attempt", strict.owner_revision);
  await click('[data-model-id="pool"]');
  await click('[data-testid="replace-model-access"]');
  await quotaForm("bounded_attempt");
  await click('[data-testid="discard-model-access"]');
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
      await quotaForm("bounded_attempt");
      await save();
      await quotaFlow();
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
