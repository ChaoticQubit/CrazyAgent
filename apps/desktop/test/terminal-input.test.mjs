import assert from "node:assert/strict";
import test from "node:test";
import { bundle, withPage } from "./browser.mjs";

const fixture = await bundle(new URL("./fixtures/one-terminal.mjs", import.meta.url).pathname);

const SAVED = [null, "terminal", "box", "both"];
const DOORS = [["agents", "Master"], ["agents", "Ada"], ["wall", "Master"], ["wall", "Ada"], ["activity", "Ada"], ["master", "Master"]];
const TERMINALS_ON_PAGE = { agents: 1, wall: 4, activity: 1, master: 1 };
const WITH_BOX = { Master: "Typing goes straight to the master.", Ada: "Typing goes straight to this agent and is not kept. Only a prompt you send below is also told to its master." };
const NO_BOX = { Master: "Typing goes straight to the master.", Ada: "Typing goes straight to this agent and is not kept." };

/** What a terminal offers under one saved Terminal input: no box with Terminal only or
 * nothing saved, one box otherwise, and a status line that promises no box it does not have.
 * Inputs: the door, the agent's name and the saved choice. Output: the counts and the line. */
function offers(door, name, saved) {
  const boxes = saved === "box" || saved === "both" ? 1 : 0;
  const note = saved === "box" ? `The terminal shows ${name}; type in the message box below.`
    : saved === "both" ? WITH_BOX[name] : `${NO_BOX[name]} Enter sends, Shift+Enter adds a line.`;
  return { boxes, sends: boxes, messages: door === "master" ? null : boxes, onPage: boxes * TERMINALS_ON_PAGE[door], note };
}

/** Opens one agent's terminal through one door of the real app with this Terminal input
 * saved. Inputs: the door, the agent's name, the saved choice and the key it is saved under.
 * Output: what the terminal offers. */
function through(door, name, saved, key = "crazyagent.terminal-input") {
  return withPage(async ({ evaluate, send }) => {
    await send("Emulation.setDeviceMetricsOverride", { width: 1200, height: 800, deviceScaleFactor: 1, mobile: false });
    await evaluate(fixture);
    await evaluate(`Fixture.saveInput(${JSON.stringify(saved)}, ${JSON.stringify(key)})`);
    await evaluate(`Fixture.openDoor(${JSON.stringify(door)}, ${JSON.stringify(name)})`);
    return evaluate(`Fixture.doorShown(${JSON.stringify(name)})`);
  });
}

for (const [door, name] of DOORS) {
  for (const saved of SAVED) {
    test(`${door}, ${name}: the terminal follows the owner's one Terminal input, saved ${saved}`, { timeout: 60_000 }, async () => {
      assert.deepEqual(await through(door, name, saved), offers(door, name, saved));
    });
  }
  test(`${door}, ${name}: an old Master input of Both left in the store is not honoured`, { timeout: 60_000 }, async () => {
    assert.deepEqual(await through(door, name, "both", "crazyagent.master-input"), offers(door, name, null));
  });
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
