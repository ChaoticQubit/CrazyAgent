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
