import assert from "node:assert/strict";
import test from "node:test";
import { onTab, press, readable, shown, walk } from "./fixtures/settings-app-tabs-run.mjs";

const PURPOSE = "CrazyAgent keeps each project in its own folder inside this one. Drop a project folder into it with Finder and it shows up on Home.";
const CHOOSE = ".set-group button";

test("Projects folder shows its title and purpose, then Folder with the place on the left and the chooser on the right", async () => {
  const seen = await onTab("home", {}, shown);
  assert.equal(seen.top?.heading, "Projects folder", JSON.stringify(seen.top));
  assert.equal(seen.top.purpose, PURPOSE);
  assert.deepEqual(seen.top.actions, []);
  assert.deepEqual(seen.sections.map((section) => section.heading), ["Folder"]);
  assert.equal(seen.sections[0].about, "Watching ~/Projects. Drop a project folder into it with Finder and it shows up here.");
  const [row] = seen.sections[0].rows;
  assert.equal(row.label, "Projects folder");
  assert.equal(row.help, "Your projects folder is ~/Projects.");
  assert.deepEqual(row.visible, ["Choose another folder"]);
  assert.ok(row.controlLeft > row.labelRight, JSON.stringify(row));
  assert.equal(row.mark, null, "this tab has no default to differ from");
});

test("a picked folder opens New projects folder with its choices, and the move asks with the size first", async () => {
  const seen = await onTab("home", {}, async (page) => {
    await press(page, CHOOSE, "Choose another folder");
    const picked = await shown(page);
    await press(page, CHOOSE, "Move my projects there");
    const asked = await page.evaluate("Fixture.dialog()");
    await press(page, "button", "Keep them here");
    return { picked, asked, closed: await page.evaluate("Fixture.dialog()") };
  });
  assert.deepEqual(seen.picked.sections.map((section) => section.heading), ["Folder", "New projects folder"], JSON.stringify(seen.picked.sections));
  for (const part of ["Use ~/Work?", "Use this folder moves nothing. Projects already in ~/Projects stay there. Move my projects there takes them with you.", "Use this folder", "Move my projects there", "Cancel"]) {
    assert.ok(seen.picked.says.includes(part), `${part} in ${seen.picked.says}`);
  }
  assert.ok(!seen.picked.says.includes("Choose another folder"), seen.picked.says);
  assert.ok(seen.asked.includes("Move your projects?"), seen.asked);
  assert.ok(seen.asked.includes("CrazyAgent moves every project folder in ~/Projects (3 GB) into ~/Work and uses ~/Work from then on. Agents can't run while it moves."), seen.asked);
  assert.equal(seen.closed, "");
});

test("a folder that cannot be used says why, below the top line", async () => {
  const seen = await onTab("home", { pick: "/Users/mira/Projects" }, async (page) => {
    await press(page, CHOOSE, "Choose another folder");
    return page.evaluate("Fixture.alert()");
  });
  assert.equal(seen.words, "This is already your projects folder.");
  assert.equal(seen.belowTop, true, JSON.stringify(seen));
});

for (const [state, scene, expected] of [
  ["no folder yet", { home: "none" }, ["No projects folder yet. Choose one to start.", "Choose folder"]],
  ["watching off", { home: "cold" }, ["~/Projects is checked when CrazyAgent starts. Live updates are off right now."]],
  ["failed", { home: "error" }, ["Try again"]],
  ["not in this build", { home: "missing" }, ["The projects folder isn't ready yet"]],
]) {
  test(`Projects folder keeps its ${state} state under the title`, async () => {
    const seen = await onTab("home", scene, shown);
    assert.equal(seen.top?.heading, "Projects folder", JSON.stringify(seen.top));
    for (const part of expected) assert.ok(seen.says.includes(part), `${part} in ${seen.says}`);
  });
}

test("real Tab keys reach the chooser, then each choice of a picked folder in order", async () => {
  const seen = await onTab("home", {}, async (page) => {
    const idle = await walk(page, 1);
    await press(page, CHOOSE, "Choose another folder");
    return { idle, picked: await walk(page, 3) };
  });
  assert.deepEqual(seen.idle, ["Choose another folder"]);
  assert.deepEqual(seen.picked, ["Use this folder", "Move my projects there", "Cancel"]);
});

for (const theme of ["light", "dark"]) {
  test(`the purpose and help read at 4.5 to 1 on Projects folder in ${theme}`, async () => {
    await onTab("home", {}, (page) => readable(page, [".settings-top p", ".set-help", ".set-head p"]), theme);
  });
}
