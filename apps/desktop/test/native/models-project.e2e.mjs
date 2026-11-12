import assert from "node:assert/strict";
import { click, finishWelcome, observeModelChoice, shot, shown, text, until } from "./settings-steps.mjs";
import { command } from "./trial4-accounts.mjs";

const UI_MS = 15000;
const HARNESS = "claude-code";
let project = null;

/** Picks one option of a select once the owner could reach it. Inputs: the select's selector
 * and the option value. */
async function choose(selector, value) {
  await until(() => shown(selector), UI_MS, `${selector} was never shown`);
  await (await $(selector)).selectByAttribute("value", value);
  await until(async () => await (await $(selector)).getValue() === value, UI_MS, `${selector} did not keep ${value}`);
}

/** Waits until the engine holds what the row expects at one scope. Inputs: the project id,
 * none for the app, the check and the label. Output: the read. */
async function engineHolds(scope, check, label) {
  let read = null;
  await until(async () => { read = await observeModelChoice(HARNESS, scope); return check(read); }, UI_MS, label);
  return read;
}

/** Sets one app-wide slot through the app's own command, naming the version the engine
 * holds. Inputs: the slot and the value. */
async function setApp(slot, value) {
  const version = (await observeModelChoice(HARNESS)).rows[slot].version;
  await command("desktop_set_model_choice", { harness: HARNESS, projectId: null, slot, value, version });
}

/** Opens the project's Harnesses section afresh, through Home, so the page reads the engine
 * again. */
async function openProjectHarnesses() {
  await browser.execute(() => { location.hash = "#/home"; });
  await until(async () => await browser.execute(() => location.hash) === "#/home", UI_MS, "Home did not open between reads");
  await browser.execute((id) => { location.hash = `#/projects/${encodeURIComponent(id)}/settings?section=harnesses`; }, project);
  await until(async () => (await text("main")).includes("Model list checked against Claude Code"), 30000, "the project Models section did not open");
}

describe("Project Settings, Harnesses, Claude Code: project model overrides on the real desktop app and engine", () => {
  it("a project with nothing set uses the app value", async () => {
    await finishWelcome("Project Models Owner");
    await click("dialog[open] button[aria-label='Close']");
    const receipt = await command("desktop_create_project", { spec: { name: "Model override", repositories: [] }, generation: 1 });
    assert.equal(receipt.outcome, "applied", `the project is made: ${JSON.stringify(receipt)}`);
    project = receipt.project_id;
    await setApp("master", "opus");
    await openProjectHarnesses();
    await until(async () => (await text("main")).includes("Uses the app value: opus."), UI_MS, "the project page did not show the app value");
    assert.equal(await (await $("#models-master")).getValue(), "", "the choice stays on Use the app value");
    assert.equal(await $("#models-add-id").isExisting(), false, "owner ids are added on the app page only");
    assert.equal(await $('div[role="radiogroup"][aria-label="What the master may pick"]').isExisting(), false, "the rules are app-wide");
    const read = await observeModelChoice(HARNESS, project);
    assert.deepEqual([read.rows.master.here, read.rows.master.effective, read.rows.master.source], [null, "opus", "app"]);
    await shot("models-project-inherit");
  });

  it("choosing a model for the project overrides the app value for this project only", async () => {
    await choose("#models-master", "sonnet");
    await click('button[aria-label="Save Master model"]');
    const read = await engineHolds(project, (read) => read.rows.master.here === "sonnet", "the engine did not hold the project model");
    assert.equal(read.rows.master.source, "project");
    assert.equal((await observeModelChoice(HARNESS)).rows.master.here, "opus", "the app value stays");
    await until(async () => (await text("main")).includes("Set for this project."), UI_MS, "the page did not say the value is the project's");
  });

  it("unticking every allowed model saves an own empty list, so only the agent default runs", async () => {
    await setApp("allowed", ["opus"]);
    await openProjectHarnesses();
    await until(async () => await (await $("label=opus")).$("input").isSelected(), UI_MS, "the inherited app list was not ticked");
    await click("label=opus");
    await click('button[aria-label="Save Allowed models"]');
    const read = await engineHolds(project, (read) => Array.isArray(read.rows.allowed.here) && read.rows.allowed.here.length === 0, "the engine did not hold an own empty list");
    assert.deepEqual([read.rows.allowed.effective, read.rows.allowed.source], [[], "project"]);
    assert.deepEqual((await observeModelChoice(HARNESS)).rows.allowed.here, ["opus"], "the app list stays");
    await until(async () => (await text("main")).includes("None ticked: only the agent default runs."), UI_MS, "the page did not say what an empty list does");
    await shot("models-project-own-empty");
  });

  it("Use the app value and Use the app list clear the project values", async () => {
    await click('button[aria-label="Use the app value, Master model"]');
    await engineHolds(project, (read) => read.rows.master.here === null && read.rows.master.source === "app", "the project master was not cleared");
    await click('button[aria-label="Use the app list, Allowed models"]');
    const read = await engineHolds(project, (read) => read.rows.allowed.here === null, "the project list was not cleared");
    assert.deepEqual([read.rows.allowed.effective, read.rows.allowed.source], [["opus"], "app"]);
    await until(async () => (await text("main")).includes("Uses the app value: opus."), UI_MS, "the page did not go back to the app value");
  });
});
