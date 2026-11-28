import assert from "node:assert/strict";
import test from "node:test";
import { bundle } from "./browser.mjs";
import { press, tabRunner } from "./settings-tab-steps.mjs";

const fixture = await bundle(new URL("./fixtures/settings-restore-switch.mjs", import.meta.url).pathname);
const onRow = tabRunner(fixture);
const SWITCH = "#log-compaction";
const sent = (page) => page.evaluate("Fixture.sent()");
const row = (page) => page.evaluate(`(() => {
  const node = document.querySelector('${SWITCH}');
  const alert = document.querySelector('[role="alert"]')?.textContent ?? null;
  return node ? { checked: node.getAttribute("aria-checked"), held: node.getAttribute("aria-disabled") === "true", alert } : { gone: true, alert };
})()`);

test("a second click on the compaction switch before its reload lands sends nothing", async () => {
  await onRow({}, async (page) => {
    await press(page, SWITCH);
    assert.deepEqual(await sent(page), { saves: ["on"], waiting: 1 }, "the save answered and asked for the reload");
    await press(page, SWITCH);
    assert.deepEqual(await sent(page), { saves: ["on"], waiting: 1 }, "no second save before the reloaded choice");
    assert.deepEqual(await row(page), { checked: "false", held: true, alert: null });
  });
});

test("once the reload lands On, a click on the compaction switch sends Off", async () => {
  await onRow({}, async (page) => {
    await press(page, SWITCH);
    await page.evaluate('Fixture.landRead("on")');
    assert.deepEqual(await row(page), { checked: "true", held: false, alert: null });
    await press(page, SWITCH);
    assert.deepEqual((await sent(page)).saves, ["on", "off"]);
  });
});

test("a failed reload after a save hides the compaction row and says nothing", async () => {
  await onRow({}, async (page) => {
    await press(page, SWITCH);
    await page.evaluate("Fixture.failRead()");
    assert.deepEqual(await row(page), { gone: true, alert: null });
  });
});
