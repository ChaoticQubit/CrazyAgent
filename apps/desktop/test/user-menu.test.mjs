import assert from "node:assert/strict";
import test from "node:test";
import { bundle } from "./browser.mjs";
import { onTab, press, readable } from "./fixtures/settings-app-tabs-run.mjs";
import { tabRunner } from "./settings-tab-steps.mjs";

const access = tabRunner(await bundle(new URL("./fixtures/settings-access-layout.mjs", import.meta.url).pathname));
const WIDTH = 1180;
const DIALOGS = [
  ["Profile", (check, theme) => access({ tab: "profile" }, check, WIDTH, theme)],
  ["Updates", (check, theme) => onTab("updates", {}, check, theme)],
  ["GitHub", (check, theme) => access({ tab: "github" }, check, WIDTH, theme)],
  ["Projects folder", (check, theme) => onTab("home", {}, check, theme)],
];

for (const [title, on] of DIALOGS) {
  for (const theme of ["light", "dark"]) {
    test(`${title} draws as one dialog in ${theme}: its title on the dialog, the panel's heading hidden, inside the window and readable`, async () => {
      const seen = await on(async (page) => {
        await readable(page, [".dialog-head h2", ".user-dialog .settings-top p"]);
        return page.evaluate("Fixture.dialogFrame()");
      }, theme);
      assert.deepEqual(seen, { title, headingHidden: true, fits: true, sideways: false, open: 1 });
    });
  }
}

test("in Projects folder, Esc on the move question closes only the question and leaves the dialog open", async () => {
  const seen = await onTab("home", {}, async (page) => {
    await press(page, ".set-group button", "Choose another folder");
    await press(page, ".set-group button", "Move my projects there");
    const asked = await page.evaluate("Fixture.dialogFrame()");
    for (const type of ["keyDown", "keyUp"]) await page.send("Input.dispatchKeyEvent", { type, key: "Escape", code: "Escape", windowsVirtualKeyCode: 27 });
    await page.evaluate("Fixture.settle(120)");
    return { asked, after: await page.evaluate("Fixture.dialogFrame()"), question: await page.evaluate("Fixture.dialog()") };
  });
  assert.equal(seen.asked.open, 2, "the question opens over the dialog");
  assert.equal(seen.after.open, 1, "Esc closed one dialog");
  assert.equal(seen.after.title, "Projects folder");
  assert.equal(seen.question, "");
});
