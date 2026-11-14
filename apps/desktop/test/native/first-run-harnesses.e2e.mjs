import assert from "node:assert/strict";
import { click, reachHarnessStep, says, shot, shown, text, texts, until } from "./settings-steps.mjs";

const box = (name) => $(`input[type=checkbox][aria-label="${name}"]`);

describe("First run asks for harnesses after tools on the real desktop app and engine, installing nothing", () => {
  it("the Harnesses step comes after Tools with only RCLI picked and locked", async () => {
    await reachHarnessStep("Harness Owner");
    assert.deepEqual(await texts(".in-steps li"), ["Profile", "Projects folder", "Tools", "Harnesses", "Accounts"]);
    await until(async () => (await text("main")).includes("Comes with CrazyAgent."), 15000, "the RCLI row did not say it comes with CrazyAgent");
    assert.equal(await box("RCLI").isSelected(), true, "RCLI starts picked");
    assert.equal(await box("RCLI").isEnabled(), false, "RCLI can't be unpicked");
    for (const name of ["Codex", "Claude Code"]) assert.equal(await box(name).isSelected(), false, `${name} starts unpicked`);
    assert.equal(await shown("button=Install picked harnesses"), false, "nothing is picked to install");
    assert.equal(await shown("button=Continue"), true, "Continue goes on with nothing picked");
    await shot("first-run-harnesses");
  });

  it("Continue with nothing picked goes on to Accounts", async () => {
    await click("button=Continue");
    await until(() => says("h1", "Add an account"), 15000, "the accounts step did not open");
  });
});
