import assert from "node:assert/strict";
import test from "node:test";
import { bundle, withPage } from "./browser.mjs";

const fixture = await bundle(new URL("./fixtures/wall-reads.mjs", import.meta.url).pathname);

const WALL_READ_WAIT_MS = 5_000;
const CLOSE = "desktop_close_project_walls";
const OPEN = "desktop_open_terminal_windows";
const GRAPH = "desktop_team_graph";

test("a wall window whose first read finds the project gone closes its project's windows", async () => {
  const closed = await withPage(async ({ evaluate }) => {
    await evaluate(fixture);
    await evaluate('Fixture.mountWall([{ fail: "project_setup_missing" }])');
    return evaluate(`Fixture.seen("${CLOSE}", 1, ${WALL_READ_WAIT_MS})`);
  });
  assert.deepEqual(closed, [{ id: "alpha" }]);
});

test("a wall window that read its team closes its project's windows once a later read finds the project gone", async () => {
  const closed = await withPage(async ({ evaluate }) => {
    await evaluate(fixture);
    await evaluate('Fixture.mountWall([{ terminals: "live" }, { fail: "project_missing" }])');
    return evaluate(`Fixture.seen("${CLOSE}", 1, ${WALL_READ_WAIT_MS})`);
  });
  assert.deepEqual(closed, [{ id: "alpha" }]);
});

test("a wall window whose read fails for another reason stays open and says why", async () => {
  const { reads, stale, closed } = await withPage(async ({ evaluate }) => {
    await evaluate(fixture);
    await evaluate('Fixture.mountWall([{ terminals: "live" }, { fail: "owner_read_failed" }])');
    const reads = await evaluate(`Fixture.seen("${GRAPH}", 2, ${WALL_READ_WAIT_MS})`);
    await evaluate(`Fixture.seen("${CLOSE}", 1, 300)`);
    return { reads: reads.length, stale: await evaluate("Fixture.stale()"), closed: await evaluate(`Fixture.calls("${CLOSE}")`) };
  });
  assert.equal(reads, 2, "the second read happened");
  assert.match(stale ?? "", /Couldn't refresh the team/u);
  assert.deepEqual(closed, [], "a failed read is not a removed project");
});

/** Clicks Open all terminals with a real mouse click on a toolbar and reads what it asked for.
 * Input: the mount expression. Output: the open calls and the graph reads it made. */
function openAll(mount) {
  return withPage(async ({ evaluate, send }) => {
    await evaluate(fixture);
    await evaluate(mount);
    const before = (await evaluate(`Fixture.calls("${GRAPH}")`)).length;
    const { x, y } = await evaluate("Fixture.openAllAt()");
    await send("Input.dispatchMouseEvent", { type: "mouseMoved", x, y });
    for (const type of ["mousePressed", "mouseReleased"]) await send("Input.dispatchMouseEvent", { type, x, y, button: "left", clickCount: 1 });
    const reads = await evaluate(`Fixture.seen("${GRAPH}", ${before + 1}, ${WALL_READ_WAIT_MS})`);
    return { opened: await evaluate(`Fixture.seen("${OPEN}", 1, 500)`), reads: reads.length - before, note: await evaluate("Fixture.note()") };
  });
}

test("Open all terminals counts the windows from a fresh read, so a page the wall opened from a newer read is kept", async () => {
  const { opened, reads } = await openAll("Fixture.mountToolbar(9, 6, [{ terminals: 13 }], 6)");
  assert.equal(reads, 1, "the click reads the team again");
  assert.deepEqual(opened, [{ id: "alpha", name: "Alpha", count: 3, focus: true }]);
});

test("Open all terminals uses the Terminals per window the engine holds now", async () => {
  const { opened } = await openAll("Fixture.mountToolbar(9, 6, [{ terminals: 9 }], 4)");
  assert.deepEqual(opened, [{ id: "alpha", name: "Alpha", count: 3, focus: true }]);
});

test("Open all terminals keeps the count it shows when the fresh read fails", async () => {
  const { opened } = await openAll('Fixture.mountToolbar(9, 6, [{ fail: "owner_read_failed" }], 6)');
  assert.deepEqual(opened, [{ id: "alpha", name: "Alpha", count: 2, focus: true }]);
});

test("Open all terminals opens nothing when the fresh read has no terminals left", async () => {
  const { opened, reads } = await openAll("Fixture.mountToolbar(9, 6, [{ terminals: 0 }], 6)");
  assert.equal(reads, 1, "the click read the team");
  assert.deepEqual(opened, []);
});

test("Open all terminals opens no window and says why when the fresh read finds the project removed (UI-5-F2)", async () => {
  for (const code of ["project_missing", "project_setup_missing"]) {
    const { opened, reads, note } = await openAll(`Fixture.mountToolbar(9, 6, [{ fail: "${code}" }], 6)`);
    assert.equal(reads, 1, `${code}: the click read the team`);
    assert.deepEqual(opened, [], `${code}: no window opens to shut again`);
    assert.match(note ?? "", /removed/u, `${code}: the row says why nothing opened`);
  }
});
