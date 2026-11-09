import assert from "node:assert/strict";
import test from "node:test";
import { bundle, withPage } from "./browser.mjs";

const fixture = await bundle(new URL("./fixtures/one-terminal.mjs", import.meta.url).pathname);

const SAVED = [null, "terminal", "box", "both"];
const SUB_NOTE = "Typing goes straight to this agent and is not kept. Only a prompt you send below is also told to its master.";

/** What a master's terminal offers under one saved Master input: no box with Terminal only
 * or nothing saved, one box otherwise. Input: the saved choice. Output: the counts and note. */
function masterOffers(saved) {
  if (saved === "box") return { boxes: 1, sends: 1, messages: 1, note: "The terminal shows Master; type in the message box below." };
  if (saved === "both") return { boxes: 1, sends: 1, messages: 1, note: "Typing goes straight to the master." };
  return { boxes: 0, sends: 0, messages: 0, note: "Typing goes straight to the master. Enter sends, Shift+Enter adds a line." };
}

/** Opens one agent's terminal through one door of the real app with this Master input saved.
 * Inputs: the door, the agent's name and the saved choice. Output: what its terminal offers. */
function through(door, name, saved) {
  return withPage(async ({ evaluate, send }) => {
    await send("Emulation.setDeviceMetricsOverride", { width: 1200, height: 800, deviceScaleFactor: 1, mobile: false });
    await evaluate(fixture);
    await evaluate(`Fixture.saveInput(${JSON.stringify(saved)})`);
    await evaluate(`Fixture.openDoor(${JSON.stringify(door)}, ${JSON.stringify(name)})`);
    return evaluate(`Fixture.doorShown(${JSON.stringify(name)})`);
  });
}

for (const door of ["agents", "wall"]) {
  for (const saved of SAVED) {
    test(`${door}: a master's terminal follows the owner's Master input, saved ${saved}`, { timeout: 60_000 }, async () => {
      assert.deepEqual(await through(door, "Master", saved), masterOffers(saved));
    });
  }
}

for (const saved of SAVED) {
  test(`the Master tab follows the owner's Master input, saved ${saved}`, { timeout: 60_000 }, async () => {
    assert.deepEqual(await through("master", "Master", saved), { ...masterOffers(saved), messages: null });
  });
}

for (const door of ["agents", "wall", "activity"]) {
  for (const saved of SAVED) {
    test(`${door}: a sub-agent keeps its message box and Message, saved ${saved}`, { timeout: 60_000 }, async () => {
      assert.deepEqual(await through(door, "Ada", saved), { boxes: 1, sends: 1, messages: 1, note: SUB_NOTE });
    });
  }
}

test("Team activity has no row for a master, so no master terminal opens there", { timeout: 60_000 }, async () => {
  const names = await withPage(async ({ evaluate }) => {
    await evaluate(fixture);
    await evaluate("Fixture.saveInput(null)");
    await evaluate('Fixture.openDoor("activity", "Ada")');
    return evaluate("Fixture.activityNames()");
  });
  assert.equal(names.length, 3, JSON.stringify(names));
  assert.ok(names.every((name) => !name.includes("Master")), JSON.stringify(names));
});
