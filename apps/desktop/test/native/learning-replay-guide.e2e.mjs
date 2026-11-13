import assert from "node:assert/strict";
import { click, observeLearningSkipped, reachAccountsStep, shot, shown, text, until } from "./settings-steps.mjs";

describe("Profile brings a skipped Learn CrazyAgent guide back on the real desktop app and engine", () => {
  it("first run with the guide unticked ends on Home with the guide skipped in the engine", async () => {
    await reachAccountsStep("Replay Guide Owner");
    await click("label=Show the guide on Home");
    await click("button=Skip for now");
    await until(() => observeLearningSkipped(), 15000, "the engine did not hold the guide as skipped");
    await until(async () => (await text("main")).includes("No projects yet"), 30000, "Home did not open");
    assert.doesNotMatch(await text("main"), /Learn CrazyAgent/, "a skipped guide is off Home");
  });

  it("Show the learning guide again clears the skip, closes Profile and shows the guide on Home", async () => {
    await browser.execute(() => { location.hash = "#/settings/profile"; });
    await until(async () => (await text("dialog[open]")).includes("Show the learning guide again"), 30000,
      "the Profile dialog did not offer the replay entries");
    await click("button=Show the learning guide again");
    await until(async () => !(await shown("dialog[open]")) && (await text("main")).includes("Learn CrazyAgent"), 30000,
      "Home did not show the guide again");
    assert.equal(await observeLearningSkipped(), false, "the engine no longer holds the guide as skipped");
    await shot("profile-show-guide-again");
  });
});
