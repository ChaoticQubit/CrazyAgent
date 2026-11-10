import assert from "node:assert/strict";
import test from "node:test";
import { bundle } from "./browser.mjs";
import { press, tabRunner } from "./settings-tab-steps.mjs";

const fixture = await bundle(new URL("./fixtures/project-menu.mjs", import.meta.url).pathname);
const onMenu = tabRunner(fixture);
const MENU = 'button[aria-label="Project actions"]';
const ITEM = '[role="menuitem"]';
const DIALOG = "dialog[open]";
const asked = (page) => page.evaluate(`document.querySelector("${DIALOG}")?.textContent ?? null`);

test("Pause all agents in the project menu says what stops, and pauses only after the confirm", async () => {
  await onMenu({ tasks: ["running", "queued"] }, async (page) => {
    await press(page, MENU);
    await press(page, ITEM, "Pause all agents");
    assert.match(await asked(page) ?? "", /1 running task stops and 1 waiting task is cancelled\./);
    assert.deepEqual(await page.evaluate("Fixture.cancelled()"), []);
    await press(page, `${DIALOG} button`, "Pause all agents");
    assert.deepEqual(await page.evaluate("Fixture.cancelled()"), [{ id: "alpha", task: null }]);
    assert.equal(await asked(page), null);
    assert.deepEqual(await page.evaluate("Fixture.told()"), ["Paused. Running tasks are stopping and waiting tasks are cancelled."]);
  });
});

test("Pause all agents with nothing running says so and asks nothing", async () => {
  await onMenu({ tasks: ["done"] }, async (page) => {
    await press(page, MENU);
    await press(page, ITEM, "Pause all agents");
    assert.equal(await asked(page), null);
    assert.deepEqual(await page.evaluate("Fixture.cancelled()"), []);
    assert.deepEqual(await page.evaluate("Fixture.told()"), ["No team agents are working or waiting."]);
  });
});
