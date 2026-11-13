import assert from "node:assert/strict";
import { existsSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { click, finishWelcome, observeModelChoice, shot, shown, text, type, until } from "./settings-steps.mjs";

const UI_MS = 15000;
const HARNESS = "claude-code";
const PAGE = `#/settings/harnesses?harness=${HARNESS}`;

/** Picks one option of a select once the owner could reach it. Inputs: the select's selector
 * and the option value. */
async function choose(selector, value) {
  await until(() => shown(selector), UI_MS, `${selector} was never shown`);
  await (await $(selector)).selectByAttribute("value", value);
  await until(async () => await (await $(selector)).getValue() === value, UI_MS, `${selector} did not keep ${value}`);
}

/** Waits until the engine holds what the row expects. Inputs: the check on the engine read
 * and the label. Output: the read. */
async function engineHolds(check, label) {
  let read = null;
  await until(async () => { read = await observeModelChoice(HARNESS); return check(read); }, UI_MS, label);
  return read;
}

async function openPage() {
  await browser.execute((hash) => { location.hash = hash; }, PAGE);
  await until(async () => (await text("main")).includes("Model list checked against Claude Code"), 30000, "the Models section did not open");
}

async function rule(label, option, field) {
  const before = (await observeModelChoice(HARNESS)).rules;
  await click(`button=${option.label}`);
  const after = await engineHolds((read) => read.rules[field] === option.value, `the engine did not hold ${field} ${option.value}`);
  assert(after.rules[`${field}_version`] > before[`${field}_version`], `${field} saved as a new version`);
  assert.equal(await (await $(`div[role="radiogroup"][aria-label="${label}"] button[aria-checked="true"]`)).getText(), option.label);
}

describe("Settings, Harnesses, Claude Code: the Models section on the real desktop app and engine", () => {
  it("shows the model list, every slot with the account's own model, the owner ids and the rules", async () => {
    await finishWelcome("Models Owner");
    await click("dialog[open] button[aria-label='Close']");
    await openPage();
    for (const label of ["Master model", "Agent default model", "Allowed models", "Your model ids", "Model rules"]) {
      assert.match(await text("main"), new RegExp(label), `${label} shows`);
    }
    assert.equal(await (await $("#models-master")).getValue(), "", "the master starts on the account's own model");
    const read = await observeModelChoice(HARNESS);
    assert.deepEqual([read.rows.master.here, read.rows.agent.here, read.rows.allowed.here], [null, null, null]);
    assert.deepEqual([read.rules.pick, read.rules.fallback], ["strict", "notice"]);
    await shot("models-app-open");
  });

  it("says the live list was not reported by Claude Code, which has no list command, and keeps the shipped ids", async () => {
    await until(async () => (await text("main")).includes("live list not reported by Claude Code."), UI_MS, "the engine's live list note did not show");
    assert(await (await $('#models-master option[value="opus"]')).isExisting(), "the shipped ids stay");
    assert(!(await (await $("button=Check again")).isExisting()), "no list command, nothing to ask again");
  });

  it("choosing a master model and Save stores it app-wide", async () => {
    await choose("#models-master", "opus");
    await click('button[aria-label="Save Master model"]');
    const read = await engineHolds((read) => read.rows.master.here === "opus", "the engine did not hold the master model");
    assert.deepEqual([read.rows.master.effective, read.rows.master.source], ["opus", "app"]);
    await until(async () => (await text("main")).includes("Set here for every project."), UI_MS, "the page did not say where the value comes from");
  });

  it("Use the account's own model clears the master model", async () => {
    const before = (await observeModelChoice(HARNESS)).rows.master.version;
    await click(`button[aria-label="Use the account's own model, Master model"]`);
    const read = await engineHolds((read) => read.rows.master.here === null, "the engine still held a master model");
    assert(read.rows.master.version > before, "the reset is a new version");
    assert.equal(await (await $("#models-master")).getValue(), "");
  });

  it("choosing an agent default and Save stores it app-wide", async () => {
    await choose("#models-agent", "sonnet");
    await click('button[aria-label="Save Agent default model"]');
    await engineHolds((read) => read.rows.agent.here === "sonnet" && read.rows.agent.source === "app", "the engine did not hold the agent default");
  });

  it("choosing Account's own model for the agent and Save clears it, never storing a null", async () => {
    await choose("#models-agent", "");
    await click('button[aria-label="Save Agent default model"]');
    const read = await engineHolds((read) => read.rows.agent.here === null, "the engine still held an agent default");
    assert.deepEqual([read.rows.agent.effective, read.rows.agent.source], [null, "default"]);
  });

  it("ticking allowed models and Save stores the list in page order", async () => {
    await click("label=haiku");
    await click("label=opus");
    await click('button[aria-label="Save Allowed models"]');
    const read = await engineHolds((read) => Array.isArray(read.rows.allowed.here) && read.rows.allowed.here.length === 2, "the engine did not hold the list");
    assert.deepEqual(read.rows.allowed.here, ["opus", "haiku"]);
  });

  it("unticking one allowed model and Save stores the shorter list", async () => {
    await click("label=haiku");
    await click('button[aria-label="Save Allowed models"]');
    await engineHolds((read) => JSON.stringify(read.rows.allowed.here) === JSON.stringify(["opus"]), "the engine did not hold the shorter list");
  });

  it("Clear the list empties the allowed list", async () => {
    await click('button[aria-label="Clear the list, Allowed models"]');
    const read = await engineHolds((read) => read.rows.allowed.here === null, "the engine still held a list");
    assert.deepEqual(read.rows.allowed.effective, []);
  });

  it("adding an own model id keeps it waiting for its test launch, never tickable", async () => {
    await type("#models-add-id", "my-model-1");
    await click("button=Add");
    const read = await engineHolds((read) => read.own.some((item) => item.id === "my-model-1"), "the engine did not keep the id");
    assert.deepEqual(read.own, [{ id: "my-model-1", origin: "pending" }]);
    await until(async () => (await text("main")).includes("my-model-1 is added. It waits for its test launch"), UI_MS, "the page did not say the id waits");
    assert.equal(await (await $("label*=my-model-1")).$("input").isEnabled(), false, "a pending id cannot be ticked");
    assert.equal(await $('#models-master option[value="my-model-1"]').isExisting(), false, "a pending id is not a choice");
    await shot("models-app-pending-id");
  });

  it("adding an id the harness's list has says so and adds nothing", async () => {
    await type("#models-add-id", "opus");
    await click("button=Add");
    await until(async () => (await text("main")).includes("opus is already on this harness's model list."), UI_MS, "the page did not say the id is listed");
    assert.deepEqual((await observeModelChoice(HARNESS)).own, [{ id: "my-model-1", origin: "pending" }]);
  });

  it("an id with characters a model id can't have is refused with the reason", async () => {
    await type("#models-add-id", "bad id!");
    await click("button=Add");
    await until(async () => (await text("main")).includes("the id has characters a model id can't have"), UI_MS, "the page did not give the reason");
    assert.deepEqual((await observeModelChoice(HARNESS)).own, [{ id: "my-model-1", origin: "pending" }]);
  });

  it("Remove takes an own id away", async () => {
    await click('button[aria-label="Remove my-model-1"]');
    await engineHolds((read) => read.own.length === 0, "the engine still kept the id");
    await until(async () => (await text("main")).includes("my-model-1 is removed."), UI_MS, "the page did not say the id is removed");
  });

  it("the pick rule Any offered model saves as hint", async () => {
    await rule("What the master may pick", { value: "hint", label: "Any offered model" }, "pick");
  });

  it("the pick rule Always the default saves as default", async () => {
    await rule("What the master may pick", { value: "default", label: "Always the default" }, "pick");
  });

  it("the pick rule Only the allowed list saves as strict", async () => {
    await rule("What the master may pick", { value: "strict", label: "Only the allowed list" }, "pick");
  });

  it("the fallback rule Stop and say why saves as stop", async () => {
    await rule("When no account can run the model", { value: "stop", label: "Stop and say why" }, "fallback");
  });

  it("the fallback rule Quietly saves as silent", async () => {
    await rule("When no account can run the model", { value: "silent", label: "Quietly" }, "fallback");
  });

  it("the fallback rule Tell me saves as notice", async () => {
    await rule("When no account can run the model", { value: "notice", label: "Tell me" }, "fallback");
    await shot("models-app-rules");
  });

  it("a harness whose model CrazyAgent does not choose shows the reason and no control", async () => {
    await browser.execute(() => { location.hash = "#/settings/harnesses?harness=opencode"; });
    await until(async () => (await text("main")).includes("CrazyAgent has no model list for"), 30000, "the OpenCode Models section did not open");
    const words = await text("main");
    assert((words.match(/Not supported by this harness/g) ?? []).length >= 3, "each slot gives the reason");
    assert.equal(await $("#models-master").isExisting(), false, "no master choice");
    assert.equal(await $("#models-add-id").isExisting(), false, "no Add field where no test launch runs");
    assert(words.includes("An id you add needs a test launch, and this harness has none."), "Add says why");
    assert.equal(await $('button[aria-label="Save Allowed models"]').isExisting(), false, "no Save");
    await shot("models-app-not-supported");
  });

  it("a Codex list read that came back unreported waits its backoff, and Check again reads it at once", async () => {
    const lists = join(process.env.CRAZYAGENT_DESKTOP_TEST_EVIDENCE, "codex.lists");
    const runs = () => existsSync(lists) ? readFileSync(lists, "utf8").split("\n").filter(Boolean).length : 0;
    const codex = async (label) => {
      await browser.execute((hash) => { location.hash = hash; }, "#/settings/harnesses?harness=codex");
      await until(async () => (await text("main")).includes("live list not reported by Codex."), 30000, label);
    };
    await codex("the Codex Models section did not say its list was not reported");
    assert.equal(runs(), 1, "the open read the list once");
    assert(await (await $("button=Check again")).isExisting(), "Codex has a list command and an executable");
    await openPage();
    await codex("the note did not show at once on the next open");
    assert.equal(runs(), 1, "a plain open inside the backoff starts no harness");
    await click("button=Check again");
    await until(async () => runs() === 2, UI_MS, "Check again did not read the list");
    await shot("models-app-check-again");
  });
});
