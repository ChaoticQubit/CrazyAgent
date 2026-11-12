import assert from "node:assert/strict";
import test from "node:test";
import { bundle, withPage } from "./browser.mjs";

const fixture = await bundle(new URL("./fixtures/terminal-wheel.mjs", import.meta.url).pathname);
const BURST = 400;
const FRAME_MS_MAX = 100;
const PAGE_MS_MAX = 15000;
const INPUT_BYTES_MAX = 4096;
const FULL_SCREEN = "\x1b[?1049h\x1b[?1002h\x1b[?1006h\x1b[H\x1b[2Jfull screen";
const WHEEL_DOWN = /\x1b\[<65;\d+;\d+M/g;
const NO_MOUSE_SCREEN = "\x1b[?2004h\x1b[?1004h\x1b[?1049h\x1b[?1007h\x1b[H\x1b[2Jfull screen, no mouse";
const FOCUS_REPORTS = /\x1b\[[IO]/g;

async function wheel(send, at, count) {
  for (let notch = 0; notch < count; notch += 1) {
    await send("Input.dispatchMouseEvent", { type: "mouseWheel", x: at.x, y: at.y, deltaX: 0, deltaY: 120 });
  }
}

async function settled(evaluate) {
  for (let waited = 0; waited < 5000; waited += 20) {
    if (await evaluate("Fixture.settled()")) return evaluate("Fixture.view()");
    await new Promise((done) => setTimeout(done, 20));
  }
  assert.fail("the sends never settled");
}

async function fullScreen(check, keys = true) {
  return withPage(async ({ evaluate, send }) => {
    await send("Emulation.setDeviceMetricsOverride", { width: 1200, height: 800, deviceScaleFactor: 1, mobile: false });
    await evaluate(fixture);
    await evaluate(`Fixture.mount(24, 80, ${keys})`);
    await evaluate(`Fixture.write(${JSON.stringify(FULL_SCREEN)})`);
    await evaluate("Fixture.frames()");
    const at = await evaluate("Fixture.view()");
    assert.deepEqual([at.kind, at.mouse], ["alternate", "drag"], "the app is full screen and reads the wheel");
    return check({ evaluate, send, at });
  });
}

test("a wheel burst on a full-screen agent while the engine is busy is sent in a few batches, never one send per event", { timeout: BURST * FRAME_MS_MAX + PAGE_MS_MAX }, async () => {
  await fullScreen(async ({ evaluate, send, at }) => {
    await evaluate("Fixture.holdSends(true)");
    await wheel(send, at, BURST);
    const busy = await evaluate("Fixture.view()");
    await evaluate("Fixture.holdSends(false)");
    const done = await settled(evaluate);
    const reports = done.text.match(WHEEL_DOWN) ?? [];
    assert.equal(done.stats.wheels, BURST, "the page saw every wheel event");
    assert.equal(reports.length, BURST, "every wheel event reaches the app");
    assert.equal(done.text, reports.join(""), "the sends hold only the wheel reports, in order");
    assert.equal(done.stats.stopped, 0, "typing never stops reaching the agent");
    const batches = Math.ceil(reports.join("").length / INPUT_BYTES_MAX) + 1;
    assert.ok(done.sends <= batches, `${done.sends} sends for ${BURST} wheel events`);
    assert.ok(busy.stats.most <= batches, `${busy.stats.most} sends waited while the engine was busy`);
  });
});

test("keys typed between wheel events keep their place among the scrolling", async () => {
  await fullScreen(async ({ evaluate, send, at }) => {
    await evaluate("Fixture.holdSends(true)");
    await wheel(send, at, 6);
    await evaluate("Fixture.focus()");
    await send("Input.dispatchKeyEvent", { type: "char", text: "q" });
    await wheel(send, at, 5);
    await evaluate("Fixture.holdSends(false)");
    const done = await settled(evaluate);
    const shape = done.text.replace(WHEEL_DOWN, "w");
    assert.equal(shape, "wwwwwwqwwwww", "scrolling never jumps ahead of a key typed after it, nor behind one typed before it");
    assert.equal(done.stats.stopped, 0);
    assert.deepEqual(done.lanes.filter((lane) => lane.startsWith("keys:")), ["keys:q"], "only the typed key goes as the owner's keys");
  });
});

test("a full-screen app that reads no mouse gets the wheel as arrow keys sent as wheel, never as the owner's keys", async () => {
  await withPage(async ({ evaluate, send }) => {
    await send("Emulation.setDeviceMetricsOverride", { width: 1200, height: 800, deviceScaleFactor: 1, mobile: false });
    await evaluate(fixture);
    await evaluate("Fixture.mount(24, 80, true)");
    await evaluate(`Fixture.write(${JSON.stringify(NO_MOUSE_SCREEN)})`);
    await evaluate("Fixture.frames()");
    const at = await evaluate("Fixture.view()");
    assert.deepEqual([at.kind, at.mouse], ["alternate", "none"], "the app is full screen and reads no mouse, as Codex does");
    await wheel(send, at, 3);
    await evaluate("Fixture.focus()");
    await send("Input.dispatchKeyEvent", { type: "char", text: "q" });
    await wheel(send, at, 2);
    const done = await settled(evaluate);
    const lane = (name) => done.lanes.filter((sent) => sent.startsWith(`${name}:`)).map((sent) => sent.slice(name.length + 1)).join("");
    assert.equal(lane("wheel"), "\x1b[B".repeat(5), "every wheel turn reached the app as an arrow key on the wheel lane");
    assert.equal(lane("keys").replace(FOCUS_REPORTS, ""), "q", "the owner's lane holds the typed key and the view's focus reports only");
  });
});

test("with the message box as the only input, keys stay out of the agent's terminal and the wheel still scrolls it", async () => {
  await fullScreen(async ({ evaluate, send, at }) => {
    await wheel(send, at, 3);
    await evaluate("Fixture.focus()");
    await send("Input.dispatchKeyEvent", { type: "char", text: "q" });
    await wheel(send, at, 2);
    const done = await settled(evaluate);
    assert.equal(done.text.replace(WHEEL_DOWN, "w"), "wwwww", "only the wheel reached the agent");
  }, false);
});
