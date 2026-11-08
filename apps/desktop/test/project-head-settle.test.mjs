import assert from "node:assert/strict";
import test from "node:test";
import { bundle, withPage } from "./browser.mjs";

const fixture = await bundle(new URL("./fixtures/project-head.mjs", import.meta.url).pathname);

test("a head whose scrolled form runs no transition and keeps its size still trims the room under the page", async () => {
  const { rest, after } = await withPage(async ({ evaluate, send }) => {
    await send("Emulation.setDeviceMetricsOverride", { width: 1280, height: 600, deviceScaleFactor: 1, mobile: false });
    await evaluate(fixture);
    await evaluate("Fixture.mountStill()");
    await evaluate("Fixture.frames(3)");
    const rest = await evaluate("Fixture.state()");
    await evaluate("document.querySelector('.main').scrollTop = 40");
    await evaluate("Fixture.frames(6)");
    return { rest, after: await evaluate("Fixture.state()") };
  });
  assert.equal(rest.head, "pj-head", `the head starts in its full form: ${JSON.stringify(rest)}`);
  assert.equal(rest.room, 0, `no room before the scroll: ${JSON.stringify(rest)}`);
  assert.equal(after.head, "pj-head scrolled", `the head takes its scrolled form: ${JSON.stringify(after)}`);
  assert.equal(after.height, rest.height, `the scrolled form keeps the head's size, so no resize follows: ${JSON.stringify({ rest, after })}`);
  assert.equal(after.room, 0, `the room ends at what the head gives up, nothing, not its full height: ${JSON.stringify({ rest, after })}`);
});

test("the meta line is drawn under the description and hidden once the head takes its scrolled form", async () => {
  const { rest, after } = await withPage(async ({ evaluate, send }) => {
    await send("Emulation.setDeviceMetricsOverride", { width: 1280, height: 600, deviceScaleFactor: 1, mobile: false });
    await evaluate(fixture);
    await evaluate('Fixture.mountStill({ folder: "~/CrazyAgent/larkspur-booking", branch: "main", access: "Full access", pool: "Pool A, 4 accounts" })');
    await evaluate("Fixture.frames(3)");
    const rest = await evaluate("Fixture.state()");
    await evaluate("document.querySelector('.main').scrollTop = 40");
    await evaluate("Fixture.frames(6)");
    return { rest, after: await evaluate("Fixture.state()") };
  });
  assert.deepEqual([rest.head, rest.meta], ["pj-head", true], `the full head draws the meta line: ${JSON.stringify(rest)}`);
  assert.deepEqual([after.head, after.meta], ["pj-head scrolled", false], `the scrolled head hides the meta line: ${JSON.stringify(after)}`);
});

/** Reads the tab strip of a project page in a window of a given width at the app's default
 * height. Input: the window's width. Output: the strip's values. */
function tabRow(width) {
  return withPage(async ({ evaluate, send }) => {
    await send("Emulation.setDeviceMetricsOverride", { width, height: 800, deviceScaleFactor: 1, mobile: false });
    await evaluate(fixture);
    await evaluate("Fixture.mountAllTabs()");
    await evaluate("Fixture.frames(3)");
    return evaluate("Fixture.tabRow()");
  });
}

test("in the default 1180x800 window every project tab stands on one row, each with its icon and word", async () => {
  const row = await tabRow(1180);
  assert.deepEqual(row.words, ["Overview", "Conversation", "Agents", "Changes", "Questions", "Context", "Process", "Metrics", "Settings"]);
  assert.equal(row.icons, row.words.length, `every tab keeps its icon: ${JSON.stringify(row)}`);
  assert.equal(new Set(row.tops).size, 1, `every tab has the same top: ${JSON.stringify(row)}`);
  assert.equal(row.sideways, false, `nothing scrolls sideways: ${JSON.stringify(row)}`);
});

test("a window at the 760 minimum may wrap the project tabs but never scrolls sideways", async () => {
  const row = await tabRow(760);
  assert.equal(row.words.length, 9);
  assert.equal(row.sideways, false, `nothing scrolls sideways: ${JSON.stringify(row)}`);
});
