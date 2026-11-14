import assert from "node:assert/strict";
import { click, reachHarnessStep, says, shot, shown, text, until } from "./settings-steps.mjs";

const NODE_MS = 600000;
const NEEDS = "codex needs node, which is missing.";

describe("First run installs Node.js for a Codex that can't run, on the real desktop app and a clean-Mac gate engine", () => {
  it("without waiting for the tools check, the Codex row says it can't run and the step offers Install Node.js", async function () {
    this.timeout(300000);
    await reachHarnessStep("Node Owner");
    await until(async () => (await text("main")).includes(NEEDS), 180000,
      "the Codex row never said it needs Node; the owner fixture must be built with --features crazyagent/clean-mac-gate");
    assert.match(await text("main"), /Can't run/);
    assert.doesNotMatch(await text("main"), /Already on this Mac/);
    await until(() => shown("button=Install Node.js"), 180000, "the step never offered Install Node.js once setup's check ended");
    await shot("first-run-harnesses-node-offer");
  });

  it("Install Node.js shows Node installing, then the Codex row leaves Can't run", async function () {
    this.timeout(NODE_MS + 60000);
    await click("button=Install Node.js");
    await until(async () => (await text("main")).includes("Installing Node.js."), 30000, "the step did not show Node.js installing");
    await until(async () => {
      const main = await text("main");
      return main.includes("Already on this Mac.") && !main.includes("Can't run") && !main.includes("Installing Node.js.");
    }, NODE_MS, "the Codex row did not leave Can't run once the engine's Node was in");
    await shot("first-run-harnesses-node-installed");
  });

  it("Continue goes on to Accounts without installing a harness", async () => {
    assert.equal(await shown("button=Install picked harnesses"), false, "nothing is picked to install");
    await click("button=Continue");
    await until(() => says("h1", "Add an account"), 15000, "the accounts step did not open");
  });
});
