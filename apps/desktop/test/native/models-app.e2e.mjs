import assert from "node:assert/strict";
import { click, finishWelcome, observeModelAccess, observeModelChoice, settingsTab, shot, shown, text, type, until } from "./settings-steps.mjs";

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

/** Waits until the engine holds what the row expects for one harness. Inputs: the harness id,
 * the check on the engine read and the label. Output: the read. */
async function harnessHolds(harness, check, label) {
  let read = null;
  await until(async () => { read = await observeModelChoice(harness); return check(read); }, UI_MS, label);
  return read;
}

/** Waits until the engine holds what the row expects for Claude Code. Inputs: the check on
 * the engine read and the label. Output: the read. */
async function engineHolds(check, label) {
  return harnessHolds(HARNESS, check, label);
}

async function openPage() {
  await browser.execute((hash) => { location.hash = hash; }, PAGE);
  await until(async () => (await text("main")).includes("Model list checked against Claude Code"), 30000, "the Models section did not open");
}

/** Picks one Native sub-agent model option, saves it and waits until the engine holds it
 * app-wide as a new version. Inputs: the harness id and the option value. Output: the read. */
async function saveNative(harness, value) {
  const before = (await observeModelChoice(harness)).rows.native.version;
  await choose("#models-native", value);
  await click('button[aria-label="Save Native sub-agent model"]');
  const read = await harnessHolds(harness, (read) => read.rows.native.here === value, `the engine did not hold ${value} for ${harness}`);
  assert.deepEqual([read.rows.native.effective, read.rows.native.source], [value, "app"]);
  assert(read.rows.native.version > before, "the choice saved as a new version");
  return read;
}

/** Clears the Native sub-agent model with Use the default and waits until the engine holds
 * nothing, so native sub-agents follow the agent's model again. Input: the harness id. */
async function clearNative(harness) {
  await click('button[aria-label="Use the default, Native sub-agent model"]');
  const read = await harnessHolds(harness, (read) => read.rows.native.here === null, `the engine still held a native sub-agent model for ${harness}`);
  assert.deepEqual([read.rows.native.effective, read.rows.native.source], ["follow", "default"]);
  assert.equal(await (await $("#models-native")).getValue(), "follow");
  await until(async () => (await text("main")).includes("Nothing set: native sub-agents follow the agent's model."), UI_MS, "the page did not say the default");
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

  it("Claude Code: the Native sub-agent model starts on Follow the agent's model with nothing set", async () => {
    assert.equal(await (await $("#models-native")).getValue(), "follow");
    assert.match(await text("main"), /Nothing set: native sub-agents follow the agent's model\./);
    const read = await observeModelChoice(HARNESS);
    assert.deepEqual([read.rows.native.here, read.rows.native.effective, read.rows.native.source], [null, "follow", "default"]);
  });

  it("Claude Code: the Native sub-agent model Let the harness choose saves as harness", async () => {
    await saveNative(HARNESS, "harness");
    await until(async () => (await text("main")).includes("so one may run a model your allowed list leaves out"), UI_MS, "the option's help did not show");
    await shot("models-app-native-harness");
  });

  it("Claude Code: the Native sub-agent model Follow the agent's model saves as follow", async () => {
    await saveNative(HARNESS, "follow");
    await until(async () => (await text("main")).includes("Every sub-agent a native lead starts runs on the lead's model."), UI_MS, "the option's help did not show");
  });

  it("Claude Code: Use the default clears the Native sub-agent model", async () => {
    await saveNative(HARNESS, "harness");
    await clearNative(HARNESS);
  });

  it("Codex: the Native sub-agent model starts on Follow the agent's model and Let the harness choose saves as harness", async () => {
    await browser.execute(() => { location.hash = "#/settings/harnesses?harness=codex"; });
    await until(async () => (await text("main")).includes("Model list checked against Codex"), 30000, "the Codex Models section did not open");
    assert.equal(await (await $("#models-native")).getValue(), "follow", "Codex starts on Follow the agent's model");
    await saveNative("codex", "harness");
    assert.equal((await observeModelChoice(HARNESS)).rows.native.here, null, "Claude Code keeps its own value");
    await shot("models-app-native-codex");
  });

  it("Codex: the Native sub-agent model Follow the agent's model saves as follow", async () => {
    await saveNative("codex", "follow");
  });

  it("Codex: Use the default clears the Native sub-agent model", async () => {
    await clearNative("codex");
  });

  it("RCLI shows the Native sub-agent model as Not supported by this harness, with no choice", async () => {
    await browser.execute(() => { location.hash = "#/settings/harnesses?harness=rcli"; });
    await until(async () => (await text("main")).includes("CrazyAgent has no model list for RCLI"), 30000, "the RCLI Models section did not open");
    assert.match(await text("main"), /Native sub-agent model/);
    assert.match(await text("main"), /Not supported by this harness/);
    assert.equal(await $("#models-native").isExisting(), false, "no native choice");
    assert.equal(await $('button[aria-label="Save Native sub-agent model"]').isExisting(), false, "no Save");
    const read = await observeModelChoice("rcli");
    assert.deepEqual([read.rows.native.here, read.rows.native.source], [null, "not_applied"]);
    await shot("models-app-native-rcli");
  });

  it("a harness whose model CrazyAgent does not choose shows the reason and no control", async () => {
    await browser.execute(() => { location.hash = "#/settings/harnesses?harness=opencode"; });
    await until(async () => (await text("main")).includes("CrazyAgent has no model list for"), 30000, "the OpenCode Models section did not open");
    const words = await text("main");
    assert((words.match(/Not supported by this harness/g) ?? []).length >= 4, "each slot gives the reason");
    assert.equal(await $("#models-native").isExisting(), false, "no native choice");
    assert.equal(await $("#models-master").isExisting(), false, "no master choice");
    assert.equal(await $("#models-add-id").isExisting(), false, "no Add field where no test launch runs");
    assert(words.includes("An id you add needs a test launch, and this harness has none."), "Add says why");
    assert.equal(await $('button[aria-label="Save Allowed models"]').isExisting(), false, "no Save");
    await shot("models-app-not-supported");
  });
});

describe("Settings, Harnesses, RCLI: the master model on the real desktop app and engine (DEC-150 M-5)", () => {
  const RCLI = "rcli";

  /** Waits until the engine holds what an RCLI row expects. Inputs: the check on the engine
   * read and the label. Output: the read. */
  async function rcliHolds(check, label) {
    let read = null;
    await until(async () => { read = await observeModelChoice(RCLI); return check(read); }, UI_MS, label);
    return read;
  }

  it("shows the RCLI model list, the master on each account's own model, and no agent slots", async () => {
    await browser.execute((hash) => { location.hash = hash; }, `#/settings/harnesses?harness=${RCLI}`);
    await until(async () => (await text("main")).includes("Model list checked against RCLI 3.0.4."), 30000, "the RCLI Models section did not open");
    assert.equal(await (await $("#models-master")).getValue(), "", "the RCLI master starts on each account's own model");
    assert.equal(await $("#models-agent").isExisting(), false, "RCLI starts no agents");
    assert((((await text("main")).match(/Not supported by this harness/g)) ?? []).length >= 2, "the agent default and allowed list give the reason");
    const read = await observeModelChoice(RCLI);
    assert.deepEqual([read.rows.master.here, read.rows.master.access.kind], [null, "editable"]);
    assert.deepEqual([read.rows.agent.access.kind, read.rows.allowed.access.kind], ["read_only", "read_only"]);
    await shot("models-rcli-open");
  });

  for (const [provider, model] of [["Anthropic", "claude-opus-5-5"], ["OpenAI", "gpt-6.1-sol"], ["OpenRouter", "anthropic/claude-opus-5.5"]]) {
    it(`an ${provider} model saves as the RCLI master model app-wide`, async () => {
      await choose("#models-master", model);
      await click('button[aria-label="Save Master model"]');
      const read = await rcliHolds((read) => read.rows.master.here === model, `the engine did not hold ${model}`);
      assert.deepEqual([read.rows.master.effective, read.rows.master.source], [model, "app"]);
      await until(async () => (await text("main")).includes("Set here for every project."), UI_MS, "the page did not say where the value comes from");
    });
  }

  it("Use the account's own model clears the RCLI master model", async () => {
    const before = (await observeModelChoice(RCLI)).rows.master.version;
    await click(`button[aria-label="Use the account's own model, Master model"]`);
    const read = await rcliHolds((read) => read.rows.master.here === null, "the engine still held an RCLI master model");
    assert(read.rows.master.version > before, "the reset is a new version");
    assert.equal(await (await $("#models-master")).getValue(), "");
    await shot("models-rcli-own");
  });

  const TEST_POOL = "Model tests";
  let testPool = null;

  it("an empty pool to run RCLI test launches on is saved", async () => {
    await settingsTab("Pools");
    await click("button=New pool");
    await type("#pool-name", TEST_POOL);
    await until(async () => (await observeModelAccess()).pools.some((pool) => pool.name === TEST_POOL), UI_MS, "the engine did not keep the test pool");
    const pool = (await observeModelAccess()).pools.find((pool) => pool.name === TEST_POOL);
    assert.deepEqual(pool.members, [], "the test pool holds no account");
    testPool = pool.id;
    await browser.execute((hash) => { location.hash = hash; }, `#/settings/harnesses?harness=${RCLI}`);
    await until(async () => (await text("main")).includes("Model list checked against RCLI 3.0.4."), 30000, "the RCLI Models section did not open again");
  });

  for (const [name, provider] of [["Anthropic API", "anthropic"], ["OpenAI API", "openai"], ["OpenRouter", "openrouter"]]) {
    it(`an id added on ${name} waits for a test launch on a ${name} account, and a pool without one says so (DEC-150 addendum 9e)`, async () => {
      assert.notEqual(testPool, null, "the test pool was saved first");
      const id = `my-${provider}-model`;
      await type("#models-add-id", id);
      assert((await text("main")).includes("Which provider runs this id?"), "Add asks which provider runs the id");
      assert.equal(await (await $("button=Add")).isEnabled(), false, "Add waits for a provider");
      await click(`input[name="models-add-provider"][value="${provider}"]`);
      await click("button=Add");
      await rcliHolds((read) => read.own.some((item) => item.id === id && item.origin === "pending"), `the engine did not keep ${id} waiting`);
      await click(`button[aria-label="Test ${id}"]`);
      await click(`input[name="models-test-pool"][value="${testPool}"]`);
      await click("button=Run test");
      await until(async () => (await text("main")).includes(`${id} still waits for its test launch. No set-up ${name} account in this pool.`), 30000,
        `the test launch did not name ${name} as the provider with no account`);
      const read = await observeModelChoice(RCLI);
      assert.deepEqual(read.own.find((item) => item.id === id), { id, origin: "pending" }, "an untested id stays pending");
      await shot(`models-rcli-test-${provider}`);
    });
  }
});
