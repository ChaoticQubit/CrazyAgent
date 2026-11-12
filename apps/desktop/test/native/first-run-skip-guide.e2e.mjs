import assert from "node:assert/strict";
import { setTimeout as pause } from "node:timers/promises";
import { addAccountFromFirstRun, click, observeLearningSkipped, reachAccountsStep, shot, text, until } from "./settings-steps.mjs";

const TICK = "label.checkbox input[type=checkbox]";
const QUIET_MS = 3000;

describe("First run lets the owner skip Learn CrazyAgent on the real desktop app and engine", () => {
  it("unticking the guide and picking Add account skips it in the engine and opens Add account", async () => {
    await reachAccountsStep("Skip Guide Owner");
    await click("label=Show the guide on Home");
    assert.equal(await $(TICK).isSelected(), false, "the guide is unticked");
    await addAccountFromFirstRun();
    await until(() => observeLearningSkipped(), 15000, "the engine did not hold the guide as skipped");
    await shot("skip-guide-add-account");
  });

  it("Home shows no Learn CrazyAgent guide once it is skipped", async () => {
    await click("dialog[open] button[aria-label='Close']");
    await browser.execute(() => { location.hash = "#/home"; });
    await until(async () => (await text("main")).includes("No projects yet"), 30000, "Home did not open");
    for (const end = Date.now() + QUIET_MS; Date.now() < end; await pause(200)) {
      assert.doesNotMatch(await text("main"), /Learn CrazyAgent/, "a skipped guide stays off Home");
    }
    assert.equal(await observeLearningSkipped(), true);
    await shot("home-without-learn-guide");
  });
});
