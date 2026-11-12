import assert from "node:assert/strict";
import { click, observeLearningSkipped, reachAccountsStep, shot, shown, text, until } from "./settings-steps.mjs";

describe("Profile runs first-run setup again on the real desktop app and engine", () => {
  it("first run with the guide unticked ends on Home with the guide skipped in the engine", async () => {
    await reachAccountsStep("Replay Setup Owner");
    await click("label=Show the guide on Home");
    await click("button=Skip for now");
    await until(() => observeLearningSkipped(), 15000, "the engine did not hold the guide as skipped");
    await until(async () => (await text("main")).includes("No projects yet"), 30000, "Home did not open");
  });

  it("Run first-run setup again clears the skip and opens first run with the saved profile", async () => {
    await browser.execute(() => { location.hash = "#/settings/profile"; });
    await until(async () => (await text("dialog[open]")).includes("Run first-run setup again"), 30000,
      "the Profile dialog did not offer the replay entries");
    await click("button=Run first-run setup again");
    await until(async () => !(await shown("dialog[open]")) && await shown("#profile-name"), 30000, "first run did not open");
    assert.equal(await $("#profile-name").getValue(), "Replay Setup Owner", "the form starts with the saved profile");
    assert.equal(await observeLearningSkipped(), false, "the engine no longer holds the guide as skipped");
    await shot("profile-run-first-run-again");
  });

  it("finishing it with the guide ticked ends on Home with the guide", async () => {
    await reachAccountsStep("Replay Setup Owner");
    await click("button=Skip for now");
    await until(async () => (await text("main")).includes("Learn CrazyAgent"), 30000, "Home did not show the guide");
    assert.equal(await observeLearningSkipped(), false, "a ticked guide writes no skip");
    await shot("home-after-first-run-again");
  });
});
