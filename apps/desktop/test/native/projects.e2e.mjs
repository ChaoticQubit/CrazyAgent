import assert from "node:assert/strict";
import { existsSync, readFileSync, renameSync, statSync, writeFileSync } from "node:fs";
import { basename, isAbsolute, join } from "node:path";
import { createNavigationReader } from "./refresh-barrier.mjs";

const evidence = process.env.CRAZYAGENT_DESKTOP_TEST_EVIDENCE;
if (!evidence || !isAbsolute(evidence)) throw new Error("private project evidence directory required");
let sequence = 0;

function readInput() {
  const path = join(evidence, "input.json");
  if (statSync(path).size > 1024 * 1024) throw new Error("project input byte bound");
  const input = JSON.parse(readFileSync(path, "utf8"));
  if (!["create", "restart"].includes(input.phase) || !Array.isArray(input.specs) || input.specs.length !== 2 ||
      typeof input.first_brief !== "string" || !input.first_brief.length || Buffer.byteLength(input.first_brief) > 8192) {
    throw new Error("closed native project input required");
  }
  for (const spec of input.specs) {
    if (typeof spec.name !== "string" || !spec.name.length || Buffer.byteLength(spec.name) > 256 ||
        !Array.isArray(spec.repositories) || spec.repositories.length !== 2 ||
        spec.repositories.some((path) => typeof path !== "string" || !isAbsolute(path) || /[\r\n\0]/.test(path))) {
      throw new Error("two explicit absolute repository declarations required");
    }
  }
  return input;
}

async function observe() {
  assert(sequence < 64, "actual observation request count bound");
  sequence += 1;
  const pending = join(evidence, `request-${sequence}.pending`);
  const reply = join(evidence, `reply-${sequence}.json`);
  writeFileSync(pending, JSON.stringify({ action: "snapshot" }), { flag: "wx", mode: 0o600 });
  renameSync(pending, join(evidence, `request-${sequence}.json`));
  await browser.waitUntil(() => existsSync(reply), { timeout: 10000, timeoutMsg: "actual bound engine observation required" });
  assert(statSync(reply).size <= 1024 * 1024, "actual observation reply byte bound");
  return JSON.parse(readFileSync(reply, "utf8"));
}

async function waitObservation(expected) {
  let actual;
  await browser.waitUntil(async () => {
    actual = await observe();
    return expected(actual);
  }, { timeout: 10000, timeoutMsg: "real committed project state required" });
  return actual;
}

async function dom(selector, field) {
  return browser.execute((selector, field) => {
    const node = selector.startsWith("button=")
      ? [...document.querySelectorAll("button")].find((node) => node.textContent.trim() === selector.slice(7))
      : document.querySelector(selector);
    if (field === "enabled") return Boolean(node && !node.disabled && node.getClientRects().length);
    if (!node) throw new Error(`actual control missing: ${selector}`);
    return field === "value" ? node.value : node.textContent.trim();
  }, selector, field);
}

async function click(selector) {
  await browser.waitUntil(async () => await dom(selector, "enabled"), { timeout: 10000 });
  assert.equal(await browser.execute((selector) => {
    const node = selector.startsWith("button=")
      ? [...document.querySelectorAll("button")].find((node) => node.textContent.trim() === selector.slice(7))
      : document.querySelector(selector);
    if (!node || node.disabled || !node.getClientRects().length) return false;
    node.click();
    return true;
  }, selector), true);
}

async function edit(selector, text) {
  await browser.waitUntil(async () => await dom(selector, "enabled"), { timeout: 10000 });
  assert.equal(await browser.execute((selector, text) => {
    const node = document.querySelector(selector);
    if (!node || node.disabled || !node.getClientRects().length) return false;
    const prototype = node instanceof HTMLInputElement ? HTMLInputElement.prototype
      : node instanceof HTMLTextAreaElement ? HTMLTextAreaElement.prototype : null;
    if (!prototype) return false;
    Object.getOwnPropertyDescriptor(prototype, "value").set.call(node, text);
    node.dispatchEvent(new Event("input", { bubbles: true }));
    return true;
  }, selector, text), true);
  await browser.waitUntil(async () => await dom(selector, "value") === text, { timeout: 10000 });
  assert.equal(await dom(selector, "value"), text);
}

async function selected(id, brief) {
  assert.match(id, /^project-[a-f0-9]{64}$/);
  await browser.waitUntil(async () => await dom(".conversation-browser .inspection .section-heading h2", "text") === id &&
    (await dom('[data-testid="conversation-freshness"]', "text")).startsWith("Brief read at revision"), { timeout: 10000 });
  assert.deepEqual(await browser.execute(() => [...document.querySelectorAll("[data-project-id].active")]
    .map((node) => node.dataset.projectId)), [id]);
  assert.equal(await dom(`[data-project-id="${id}"] .muted`, "text"), "Declared repositories: 2");
  assert.equal(await dom('[data-testid="current-project-brief"]', "text"), brief ?? "No brief has been recorded.");
}

function checkState(actual, specs, secondId, firstBrief) {
  assert.equal(actual.projects.length, 2);
  assert.deepEqual(actual.tasks, []);
  assert.deepEqual(actual.baselines, []);
  for (const [index, spec] of specs.entries()) {
    const project = actual.projects.find((project) => project.spec.name === spec.name);
    assert(project, "actual project spec must be present");
    assert.deepEqual(project.spec, spec);
    assert.match(project.id, /^project-[a-f0-9]{64}$/);
    if (index === 0) {
      assert.notEqual(project.id, secondId);
      assert.equal(project.brief, null);
      assert.deepEqual(project.history, []);
    } else {
      assert.equal(project.id, secondId);
      assert.deepEqual(project.brief, { version: "1", text: firstBrief });
      assert.equal(project.history.length, 1);
      assert.equal(project.history[0].text, firstBrief);
      assert.deepEqual(project.history[0].content, { kind: "Brief", version: 1 });
    }
  }
}

async function nativeIdentity() {
  const read = createNavigationReader(evidence, basename(evidence));
  let actual;
  await browser.waitUntil(() => {
    actual = read();
    return actual.finished === 1 && actual.sequence === 2 && !actual.partial;
  }, { timeout: 10000, timeoutMsg: "actual first native document lifetime required" });
  assert.equal(actual.generation, 1);
  assert.equal(actual.pending, false);
  assert.match(actual.identity, /^[1-9][0-9]*:[1-9][0-9]*$/);
  return actual.identity;
}

describe("actual native project creation and full process restart", () => {
  it("creates through the form or selects its persisted exact project after restart", async () => {
    const input = readInput();
    const identity = await nativeIdentity();
    let actual = await observe();
    assert.deepEqual(actual.tasks, []);
    assert.deepEqual(actual.baselines, []);
    let secondId;
    if (input.phase === "create") {
      assert.deepEqual(actual.projects, []);
      for (const [index, spec] of input.specs.entries()) {
        const previous = new Set(actual.projects.map((project) => project.id));
        await click("button=Projects");
        if (index === 0) {
          await browser.waitUntil(async () => (await dom(".baseline-list", "text")).includes("No projects were returned."), { timeout: 10000 });
          assert.deepEqual(await browser.execute(() => [...document.querySelectorAll("[data-project-id]")]
            .map((node) => node.dataset.projectId)), []);
        }
        await edit("#project-name", spec.name);
        await edit("#project-repositories", spec.repositories.join("\n"));
        await browser.waitUntil(async () => (await dom('form[aria-label="Create project"]', "text")).includes("2 declared repositories"), { timeout: 10000 });
        assert((await dom('form[aria-label="Create project"]', "text")).includes("2 declared repositories"));
        await click('form[aria-label="Create project"] button[type="submit"]');
        actual = await waitObservation((value) => value.projects.length === index + 1);
        const added = actual.projects.filter((project) => !previous.has(project.id));
        assert.equal(added.length, 1);
        assert.deepEqual(added[0].spec, spec);
        assert.equal(added[0].brief, null);
        assert.deepEqual(added[0].history, []);
        assert.deepEqual(actual.tasks, []);
        assert.deepEqual(actual.baselines, []);
        await selected(added[0].id, null);
        secondId = added[0].id;
      }
      assert(secondId, "second real form receipt must select an observed native ID");
      await click('[data-testid="load-current-brief"]');
      await browser.waitUntil(async () => (await dom(".inspection", "text")).includes("Editor based on version 0."), { timeout: 10000 });
      assert((await dom(".inspection", "text")).includes("Editor based on version 0."));
      await edit('[data-testid="conversation-brief"]', input.first_brief);
      await click('[data-testid="replace-project-brief"]');
      actual = await waitObservation((value) => value.projects.find((project) => project.id === secondId)?.brief?.version === "1");
    } else {
      assert.deepEqual(actual, input.persisted);
      secondId = actual.projects.find((project) => project.spec.name === input.specs[1].name)?.id;
      assert(secondId, "persisted actual second ID required");
      await click("button=Projects");
      await click(`[data-project-id="${secondId}"]`);
      await selected(secondId, input.first_brief);
      const firstId = actual.projects.find((project) => project.spec.name === input.specs[0].name)?.id;
      assert(firstId && firstId !== secondId, "first actual project is a negative selection control");
      await click(`[data-project-id="${firstId}"]`);
      await selected(firstId, null);
      await click(`[data-project-id="${secondId}"]`);
    }
    await browser.waitUntil(async () => await dom('[data-testid="current-project-brief"]', "text") === input.first_brief, { timeout: 10000 });
    await selected(secondId, input.first_brief);
    assert((await dom(".inspection", "text")).includes("Current brief (version 1)"));
    await browser.waitUntil(async () => await dom('[data-testid="conversation-entry-text"]', "text") === input.first_brief, { timeout: 10000 });
    assert.equal(await dom('[data-testid="conversation-entry-text"]', "text"), input.first_brief);
    actual = await observe();
    checkState(actual, input.specs, secondId, input.first_brief);
    if (input.phase === "restart") assert.deepEqual(actual, input.persisted);
    const screenshot = join(evidence, "project-selected.png");
    await browser.saveScreenshot(screenshot);
    assert(statSync(screenshot).size <= 2 * 1024 * 1024, "project screenshot byte bound");
    const result = JSON.stringify({ phase: input.phase, app_identity: identity, selected_id: secondId, actual });
    assert(Buffer.byteLength(result) <= 1024 * 1024, "actual UI proof byte bound");
    writeFileSync(join(evidence, "ui-result.json"), result, { flag: "wx", mode: 0o600 });
  });
});
