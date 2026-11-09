import assert from "node:assert/strict";
import test from "node:test";
import { bundle } from "./browser.mjs";
import { press, tabRunner } from "./settings-tab-steps.mjs";

const fixture = await bundle(new URL("./fixtures/team-rows-save.mjs", import.meta.url).pathname);
const onRows = tabRunner(fixture);
const SAVE = "desktop_update_project_setup";
const ASSIGN = "desktop_edit_model_access";
const MORE = 'button[aria-label="More Codex agents"]';
const DIALOG = "dialog[open]";
const sent = async (page, name) => (await page.evaluate("Fixture.commands()")).filter((command) => command === name).length;
const state = (page) => page.evaluate(`(() => {
  const more = document.querySelector('${MORE}');
  return { disabled: more.disabled, shown: more.parentElement.querySelector("output").textContent, alert: document.querySelector('[role="alert"]')?.textContent ?? null };
})()`);

test("after a harness save the row waits for the read, so a quick second click sends nothing and is not refused", async () => {
  await onRows({}, async (page) => {
    await press(page, MORE);
    assert.deepEqual(await state(page), { disabled: true, shown: "3", alert: null });
    await press(page, MORE);
    assert.equal(await sent(page, SAVE), 1);
  });
});

test("when the read lands the harness row can be changed again", async () => {
  await onRows({}, async (page) => {
    await press(page, MORE);
    await page.evaluate("Fixture.landRead()");
    assert.equal((await state(page)).disabled, false);
  });
});

test("a read at the same revision also lets the harness row go, so it can never stay stuck", async () => {
  await onRows({}, async (page) => {
    await press(page, MORE);
    await page.evaluate("Fixture.landSameRevision()");
    assert.equal((await state(page)).disabled, false);
  });
});

test("a refused save frees the harness row at once and says why", async () => {
  await onRows({ refused: true }, async (page) => {
    await press(page, MORE);
    const seen = await state(page);
    assert.equal(seen.disabled, false, JSON.stringify(seen));
    assert.equal(seen.shown, "2", JSON.stringify(seen));
    assert.notEqual(seen.alert, null, JSON.stringify(seen));
  });
});

test("choosing No pool asks first and changes nothing yet", async () => {
  await onRows({}, async (page) => {
    await page.evaluate('Fixture.choose("account", "")');
    assert.match(await page.evaluate(`document.querySelector("${DIALOG}")?.textContent ?? ""`), /The master and agents wait and show Pick a pool to start\./);
    assert.equal(await sent(page, ASSIGN), 0);
    assert.equal(await page.evaluate('document.getElementById("account").value'), "pool-a");
  });
});

test("Keep this pool closes the question and sends nothing", async () => {
  await onRows({}, async (page) => {
    await page.evaluate('Fixture.choose("account", "")');
    await press(page, `${DIALOG} button`, "Keep this pool");
    assert.equal(await page.evaluate(`document.querySelector("${DIALOG}") === null`), true);
    assert.equal(await sent(page, ASSIGN), 0);
    assert.equal(await page.evaluate('document.getElementById("account").value'), "pool-a");
  });
});

test("Use no pool takes the project off its pool", async () => {
  await onRows({}, async (page) => {
    await page.evaluate('Fixture.choose("account", "")');
    await press(page, `${DIALOG} button`, "Use no pool");
    assert.deepEqual(await page.evaluate("Fixture.assignedPools()"), [null]);
    assert.equal((await state(page)).alert, null);
  });
});

test("picking another pool does not ask", async () => {
  await onRows({}, async (page) => {
    await page.evaluate('Fixture.choose("account", "pool-b")');
    assert.equal(await page.evaluate(`document.querySelector("${DIALOG}") === null`), true);
    assert.deepEqual(await page.evaluate("Fixture.assignedPools()"), ["pool-b"]);
  });
});
