import assert from "node:assert/strict";
import test from "node:test";
import { onTab, press, readable, shown } from "./fixtures/settings-app-tabs-run.mjs";

const LABELS = [
  "Closing tickets", "Tickets agents create", "What agents may change", "Cycle length",
  "Unfinished work at cycle close", "Roadmap import", "Manual check results", "CI check results",
];
const EFFECT = "Changes apply to the next ticket action. Tickets already closed stay closed.";
const note = (page) => page.evaluate("document.querySelector('#mount .set-body [role=\"status\"]')?.textContent ?? ''");

test("Tickets shows its title, then Ticket rules with each setting's words above its choices", async () => {
  const seen = await onTab("tickets", {}, shown);
  assert.equal(seen.top?.heading, "Tickets", JSON.stringify(seen.top));
  assert.ok(seen.top.purpose.includes("Project Settings, Tickets"), JSON.stringify(seen.top));
  assert.deepEqual(seen.top.actions, []);
  assert.deepEqual(seen.sections.map((section) => section.heading), ["Ticket rules"]);
  assert.equal(seen.sections[0].about, EFFECT);
  const rows = seen.sections[0].rows;
  assert.deepEqual(rows.map((row) => row.label), LABELS);
  assert.equal(rows[0].help, "Who or what closes a ticket once its work is done.");
  assert.deepEqual(rows.map((row) => row.radios), [4, 4, 4, 6, 4, 3, 3, 4], "the default, every option and, for a cycle, Custom");
  for (const row of rows) assert.ok(row.controlTop >= row.labelBottom, `the choices stand under the words: ${JSON.stringify(row)}`);
  for (const part of ["Default (When the evidence is in)", "2 weeks", "Custom", "Save changes"]) assert.ok(seen.says.includes(part), `${part} in ${seen.says}`);
  assert.equal(seen.says.includes("Use app default"), false, "only a project page clears its values");
});

test("the changed mark shows only away from the default, for a stored value and a fresh pick", async () => {
  const marks = (page) => page.evaluate("Fixture.sections()[0].rows.map((row) => row.mark)");
  const fresh = await onTab("tickets", {}, async (page) => {
    const before = await marks(page);
    await press(page, 'input[name="ts-create"][value="direct"]');
    return { before, picked: await marks(page) };
  });
  assert.deepEqual(fresh.before, Array(8).fill(null));
  assert.deepEqual(fresh.picked, [null, "changed", null, null, null, null, null, null]);
  const stored = await onTab("tickets", { ticketsHere: { close: "owner", cycle_days: 14 } }, marks);
  assert.deepEqual(stored, ["changed", null, null, null, null, null, null, null], "a stored value equal to the default is no change");
});

test("Save changes waits for a change, and a refused save keeps its note in the section", async () => {
  const seen = await onTab("tickets", {}, async (page) => {
    const idle = await page.evaluate("document.querySelector('#mount button[type=\"submit\"]').disabled");
    await press(page, 'input[name="ts-close"][value="owner"]');
    const ready = await page.evaluate("document.querySelector('#mount button[type=\"submit\"]').disabled");
    await press(page, 'button[type="submit"]');
    return { idle, ready, note: await note(page) };
  });
  assert.equal(seen.idle, true);
  assert.equal(seen.ready, false);
  assert.ok(seen.note.length > 0, JSON.stringify(seen));
});

for (const [state, scene, expected] of [
  ["failed", { tickets: "error" }, ["Try again"]],
  ["not in this build", { tickets: "missing" }, ["Ticket settings isn't ready yet"]],
]) {
  test(`Tickets keeps its ${state} state under the title`, async () => {
    const seen = await onTab("tickets", scene, shown);
    assert.equal(seen.top?.heading, "Tickets", JSON.stringify(seen.top));
    for (const part of expected) assert.ok(seen.says.includes(part), `${part} in ${seen.says}`);
  });
}

for (const theme of ["light", "dark"]) {
  test(`the purpose, help and changed mark read at 4.5 to 1 on Tickets in ${theme}`, async () => {
    await onTab("tickets", { ticketsHere: { close: "owner" } }, (page) => readable(page, [".settings-top p", ".set-head p", ".set-help", ".set-changed"]), theme);
  });
}
