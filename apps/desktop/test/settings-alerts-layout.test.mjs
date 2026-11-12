import assert from "node:assert/strict";
import test from "node:test";
import { onTab, readable, shown, walk } from "./fixtures/settings-app-tabs-run.mjs";

test("Team alerts shows its title, the wake rule, then one section per project with a removed project named so", async () => {
  const seen = await onTab("alerts", {}, async (page) => ({ ...(await shown(page)), noteInset: await page.evaluate("(() => { const note = document.querySelector('.set-body [role=note]'); if (!note) return null; const words = document.createRange(); words.selectNodeContents(note); return words.getBoundingClientRect().left - note.closest('.set-body').getBoundingClientRect().left; })()") }));
  assert.equal(seen.top?.heading, "Team alerts", JSON.stringify(seen.top));
  assert.ok(seen.top.purpose.length > 0, JSON.stringify(seen.top));
  assert.equal(seen.noteInset, 16, "the note lines up with the rule's switches");
  assert.deepEqual(seen.top.actions, []);
  assert.deepEqual(seen.sections.map((section) => section.heading), ["Wake the master", "Alpha", "A removed project"]);
  assert.equal(seen.sections[0].about, "Sub-agent events wake each project's master on their own. Projects and agents can set their own rule on the Agents tab.");
  for (const part of ["When a question waits for you while CrazyAgent is in the background", "Note 3", "Note 2", "Note 1", "Mark 2 as seen", "Mark 1 as seen"]) {
    assert.ok(seen.says.includes(part), `${part} in ${seen.says}`);
  }
  assert.ok(seen.says.indexOf("Note 3") < seen.says.indexOf("A removed project"), seen.says);
  assert.ok(!seen.says.includes("No alerts"), seen.says);
});

for (const [state, scene, expected] of [
  ["no alerts", { events: "none" }, ["No alerts", "Wake the master"]],
  ["failed", { events: "error" }, ["Couldn't load team alerts.", "Try again"]],
  ["not in this build", { events: "missing" }, ["Team alerts isn't ready yet"]],
]) {
  test(`Team alerts keeps its ${state} state under the title`, async () => {
    const seen = await onTab("alerts", scene, shown);
    assert.equal(seen.top?.heading, "Team alerts", JSON.stringify(seen.top));
    for (const part of expected) assert.ok(seen.says.includes(part), `${part} in ${seen.says}`);
  });
}

test("real Tab keys go through the wake rule and then each project's Mark as seen in page order", async () => {
  const seen = await onTab("alerts", {}, (page) => walk(page, 14));
  const first = seen.indexOf("Mark 2 as seen");
  const second = seen.indexOf("Mark 1 as seen");
  assert.ok(first > 0, `the wake rule comes first: ${JSON.stringify(seen)}`);
  assert.ok(second > first, JSON.stringify(seen));
});

for (const theme of ["light", "dark"]) {
  test(`the purpose and section words read at 4.5 to 1 on Team alerts in ${theme}`, async () => {
    await onTab("alerts", {}, (page) => readable(page, [".settings-top p", ".set-head p"]), theme);
  });
}
