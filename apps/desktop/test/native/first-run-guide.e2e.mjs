import assert from "node:assert/strict";
import { click, observeLearningSkipped, reachAccountsStep, says, shot, shown, text, until } from "./settings-steps.mjs";

const TICK = "label.checkbox input[type=checkbox]";

describe("First run offers Learn CrazyAgent on the real desktop app and engine, guide kept", () => {
  it("the last page offers the guide ticked under Add account and Skip for now", async () => {
    await reachAccountsStep("Guide Owner");
    assert.equal(await says("#learn-offer-title", "Learn CrazyAgent"), true);
    assert.equal(await $(TICK).isSelected(), true, "the guide starts ticked");
    await shot("first-run-learn-offer");
  });

  it("Skip for now with the guide ticked opens Home with the guide and skips nothing in the engine", async () => {
    await click("button=Skip for now");
    await until(async () => !(await shown("#learn-offer-title")) && (await text("main")).includes("Learn CrazyAgent"),
      30000, "Home did not show the Learn CrazyAgent guide");
    assert.equal(await observeLearningSkipped(), false, "a ticked guide writes no skip");
    await shot("home-with-learn-guide");
  });
});
