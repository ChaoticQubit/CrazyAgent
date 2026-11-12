import assert from "node:assert/strict";
import test from "node:test";
import { onTab, press, readable, shown } from "./fixtures/settings-app-tabs-run.mjs";

const LABELS = [
  "Checks before Accept", "Runs at once", "Failed snapshots kept", "Agent run requests",
  "Lost tests", "New skip markers", "Protected paths", "Tests that stopped running",
];
const EFFECT = "Changes apply to check runs queued after you save and to the next Accept.";
const note = (page) => page.evaluate("document.querySelector('#mount .set-body [role=\"status\"]')?.textContent ?? ''");

test("Checks shows its title, then Check rules with each setting's words above its choices", async () => {
  const seen = await onTab("checks", {}, shown);
  assert.equal(seen.top?.heading, "Checks", JSON.stringify(seen.top));
  assert.ok(seen.top.purpose.includes("Project Settings, Checks"), JSON.stringify(seen.top));
  assert.deepEqual(seen.top.actions, []);
  assert.deepEqual(seen.sections.map((section) => section.heading), ["Check rules"]);
  assert.equal(seen.sections[0].about, EFFECT);
  const rows = seen.sections[0].rows;
  assert.deepEqual(rows.map((row) => row.label), LABELS);
  assert.equal(rows[0].help, "What Accept asks of the change set's checks.");
  assert.deepEqual(rows.map((row) => row.radios), [4, 5, 7, 5, 4, 4, 3, 4], "the default and every option");
  for (const row of rows) assert.ok(row.controlTop >= row.labelBottom, `the choices stand under the words: ${JSON.stringify(row)}`);
  for (const part of ["Default (Warn)", "Save changes"]) assert.ok(seen.says.includes(part), `${part} in ${seen.says}`);
  assert.equal(seen.says.includes("Use app default"), false, "only a project page clears its values");
});

test("the changed mark shows only away from the default, for a stored value and a fresh pick", async () => {
  const marks = (page) => page.evaluate("Fixture.sections()[0].rows.map((row) => row.mark)");
  const fresh = await onTab("checks", {}, async (page) => {
    const before = await marks(page);
    await press(page, 'input[name="cs-parallel"][value="3"]');
    return { before, picked: await marks(page) };
  });
  assert.deepEqual(fresh.before, Array(8).fill(null));
  assert.deepEqual(fresh.picked, [null, "changed", null, null, null, null, null, null]);
  const stored = await onTab("checks", { checksHere: { gate: "require", parallel: 1 } }, marks);
  assert.deepEqual(stored, ["changed", null, null, null, null, null, null, null], "a stored value equal to the default is no change");
});

test("Save changes waits for a change, and a refused save keeps its note in the section", async () => {
  const seen = await onTab("checks", {}, async (page) => {
    const idle = await page.evaluate("document.querySelector('#mount button[type=\"submit\"]').disabled");
    await press(page, 'input[name="cs-gate"][value="require"]');
    const ready = await page.evaluate("document.querySelector('#mount button[type=\"submit\"]').disabled");
    await press(page, 'button[type="submit"]');
    return { idle, ready, note: await note(page) };
  });
  assert.equal(seen.idle, true);
  assert.equal(seen.ready, false);
  assert.ok(seen.note.length > 0, JSON.stringify(seen));
});

for (const [state, scene, expected] of [
  ["failed", { checks: "error" }, ["Try again"]],
  ["not in this build", { checks: "missing" }, ["Check settings isn't ready yet"]],
]) {
  test(`Checks keeps its ${state} state under the title`, async () => {
    const seen = await onTab("checks", scene, shown);
    assert.equal(seen.top?.heading, "Checks", JSON.stringify(seen.top));
    for (const part of expected) assert.ok(seen.says.includes(part), `${part} in ${seen.says}`);
  });
}

for (const theme of ["light", "dark"]) {
  test(`the purpose, help and changed mark read at 4.5 to 1 on Checks in ${theme}`, async () => {
    await onTab("checks", { checksHere: { gate: "require" } }, (page) => readable(page, [".settings-top p", ".set-head p", ".set-help", ".set-changed"]), theme);
  });
}
