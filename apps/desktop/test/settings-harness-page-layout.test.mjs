import assert from "node:assert/strict";
import test from "node:test";
import { CONTRAST_MIN, contrasts, onTab, read, say, top } from "./settings-agents-page.mjs";

const on = (scene, check, theme) => onTab("harnesses", { harness: "codex", ...scene }, check, 1180, theme);

test("a harness page shows the harness as its title with All harnesses at the right, then its install and one section per group", async () => {
  const seen = await on({}, async (page) => ({ top: await top(page), sections: await read(page) }));
  assert.equal(seen.top?.heading, "Codex", JSON.stringify(seen.top));
  assert.ok(seen.top.purpose.includes("Project Settings, Harnesses"), JSON.stringify(seen.top));
  assert.deepEqual(seen.top.actions, ["All harnesses"]);
  assert.deepEqual(seen.sections.map((section) => section.heading), ["On this Mac", "Settings (1)", "Set by CrazyAgent (1)", "Read-only (1)"]);
  assert.deepEqual(seen.sections[0].rows.map((row) => row.label), ["Found at", "Runs in the background with", "Model"]);
  assert.equal(seen.sections[0].rows[0].control, "~/.local/bin/codex");
  const [setting] = seen.sections[1].rows;
  assert.equal(setting.label, "Hide reasoning");
  assert.ok(setting.help.includes("hide_agent_reasoning") && setting.help.includes("Hides the model's reasoning"), setting.help);
  assert.ok(setting.controlLeft > setting.labelRight, `the control stands right of the words: ${JSON.stringify(setting)}`);
  const [locked] = seen.sections[2].rows;
  assert.ok(locked.control.includes("danger-full-access") && locked.control.includes("sets this itself"), locked.control);
  assert.deepEqual(locked.buttons, [], "a key CrazyAgent sets offers no button");
});

test("the version line and when changes apply stay above the groups", async () => {
  const words = await on({}, say);
  assert.ok(words.indexOf("Codex 0.50.0 is qualified.") < words.indexOf("Settings (1)"), words);
  assert.ok(words.includes("Changes apply the next time a master or agent starts with this harness."), words);
});

for (const [state, scene, expected] of [
  ["a failed harness list", { harnesses: "error" }, "Couldn't read the harness list."],
  ["a failed settings read", { harnessPage: "error" }, "Try again"],
]) {
  test(`the page keeps its title and way back with ${state}`, async () => {
    const seen = await on(scene, async (page) => ({ top: await top(page), words: await say(page) }));
    assert.deepEqual(seen.top?.actions, ["All harnesses"], JSON.stringify(seen.top));
    assert.ok(seen.words.includes(expected), seen.words);
  });
}

for (const theme of ["light", "dark"]) {
  test(`the purpose and help read at 4.5 to 1 on a harness page in ${theme}`, async () => {
    const ratios = await on({}, (page) => contrasts(page, [".settings-top p", ".set-help"]), theme);
    for (const [selector, ratio] of Object.entries(ratios)) assert.ok(ratio >= CONTRAST_MIN, `${selector}: ${ratio}`);
  });
}
