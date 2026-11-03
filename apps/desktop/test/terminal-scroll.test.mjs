import assert from "node:assert/strict";
import test from "node:test";
import { bundle, withPage } from "./browser.mjs";

const fixture = await bundle(new URL("./fixtures/terminal-scroll.mjs", import.meta.url).pathname);
const ROWS = 10;
const TURNS = 400;

const lines = (from, count) => Array.from({ length: count }, (_, index) => `line ${from + index}`);
const history = (rows, count) => `\x1b[0m\x1b[H\x1b[2J${rows.map((row) => `${row}\r\n`).join("")}\x1b[0m${"\n".repeat(count - 1)}`;
const screen = (rows) => `\x1b[0m\x1b[H\x1b[2J${rows.join("\r\n")}\x1b[0m\x1b[${rows.length};1H`;

async function wheel(send, evaluate, at, deltaY, goal) {
  let seen = await evaluate("Fixture.view()");
  for (let turn = 0; turn < TURNS && seen.viewportY !== goal; turn += 1) {
    await send("Input.dispatchMouseEvent", { type: "mouseWheel", x: at.x, y: at.y, deltaX: 0, deltaY });
    seen = await evaluate("Fixture.view()");
  }
  await evaluate("Fixture.frames()");
  return evaluate("Fixture.view()");
}

async function scrolled(output) {
  return withPage(async ({ evaluate, send }) => {
    await send("Emulation.setDeviceMetricsOverride", { width: 1200, height: 800, deviceScaleFactor: 1, mobile: false });
    await evaluate(fixture);
    await evaluate(`Fixture.mount(${ROWS})`);
    await evaluate(`Fixture.write(${JSON.stringify(output)})`);
    await evaluate("Fixture.frames()");
    const before = await evaluate("Fixture.view()");
    const up = await wheel(send, evaluate, before, -240, 0);
    const down = await wheel(send, evaluate, before, 240, before.baseY);
    return { before, up, down };
  });
}

test("the wheel scrolls the team terminal up through the history sent on attach and back to the live screen", async () => {
  const seen = await scrolled(history(lines(1, 290), ROWS) + screen(lines(291, ROWS)));
  assert.deepEqual(seen.before, { ...seen.before, kind: "normal", viewportY: 290, baseY: 290, length: 300, top: "line 291", sent: [] });
  assert.equal(seen.up.top, "line 1", "the wheel reaches the pane's first history line");
  assert.equal(seen.up.viewportY, 0);
  assert.equal(seen.down.top, "line 291", "the wheel comes back down to the live screen");
  assert.equal(seen.down.viewportY, 290);
  assert.deepEqual([...seen.up.sent, ...seen.down.sent], [], "scrolling never types into the agent");
});

test("a screen-only attach, as the engine sent before, leaves the wheel nothing to scroll", async () => {
  const seen = await scrolled(screen(lines(291, ROWS)));
  assert.deepEqual([seen.before.length, seen.up.viewportY, seen.up.top], [ROWS, 0, "line 291"]);
});

test("Jump to latest shows only while the owner reads above the live screen, never on a fresh view or once the app goes full screen", async () => {
  const seen = await withPage(async ({ evaluate, send }) => {
    await send("Emulation.setDeviceMetricsOverride", { width: 1200, height: 800, deviceScaleFactor: 1, mobile: false });
    await evaluate(fixture);
    await evaluate(`Fixture.mount(${ROWS})`);
    await evaluate(`Fixture.write(${JSON.stringify(history(lines(1, 290), ROWS) + screen(lines(291, ROWS)))})`);
    await evaluate("Fixture.frames()");
    await evaluate("Fixture.follow()");
    const fresh = await evaluate("Fixture.reading()");
    const before = await evaluate("Fixture.view()");
    const up = await wheel(send, evaluate, before, -240, 0);
    const reading = await evaluate("Fixture.reading()");
    await evaluate(`Fixture.write(${JSON.stringify("\x1b[?1049h\x1b[H\x1b[2Jfull screen")})`);
    await evaluate("Fixture.frames()");
    return { fresh, top: up.top, reading, full: await evaluate("Fixture.reading()"), kind: (await evaluate("Fixture.view()")).kind };
  });
  assert.deepEqual(seen, { fresh: false, top: "line 1", reading: true, full: false, kind: "alternate" });
});
