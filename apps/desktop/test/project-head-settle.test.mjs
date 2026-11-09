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

for (const [label, reduced] of [["with motion", false], ["with motion reduced", true]]) {
  test(`a page just taller than the window keeps the head's scrolled form ${label}`, async () => {
    for (const over of [10, 20]) {
      const seen = await withPage(async ({ evaluate, send }) => {
        await send("Emulation.setDeviceMetricsOverride", { width: 1280, height: 600, deviceScaleFactor: 1, mobile: false });
        await send("Emulation.setEmulatedMedia", { features: [{ name: "prefers-reduced-motion", value: reduced ? "reduce" : "no-preference" }] });
        await evaluate(fixture);
        const range = await evaluate(`Fixture.mountShort(${over})`);
        const duration = await evaluate(`getComputedStyle(document.documentElement).getPropertyValue("--dur-base").trim()`);
        const rest = await evaluate("Fixture.state()");
        await evaluate(`document.querySelector('.main').scrollTop = ${over}`);
        await evaluate("Fixture.frames(20)");
        const after = await evaluate("Fixture.state()");
        await evaluate("new Promise((done) => setTimeout(done, 500))");
        return { range, duration, rest, after, later: await evaluate("Fixture.state()") };
      });
      const shown = `${over} px: ${JSON.stringify(seen)}`;
      assert.equal(seen.range, over, `the page is that much taller: ${shown}`);
      assert.equal(seen.duration, reduced ? "0ms" : "180ms", `the page runs in this motion setting: ${shown}`);
      assert.ok(seen.after.height < seen.rest.height, `the head gives up height: ${shown}`);
      assert.equal(seen.after.head, "pj-head scrolled", `the head takes its scrolled form: ${shown}`);
      assert.equal(seen.later.head, "pj-head scrolled", `the head does not snap back: ${shown}`);
      assert.ok(seen.later.top > 2, `the browser did not pull the page back to the top: ${shown}`);
      assert.equal(seen.later.room, Math.ceil(seen.rest.height - seen.later.height), `the room is what the head gave up: ${shown}`);
    }
  });
}

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
