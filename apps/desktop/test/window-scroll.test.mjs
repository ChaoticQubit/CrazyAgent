import assert from "node:assert/strict";
import test from "node:test";
import { bundle, withPage } from "./browser.mjs";

const fixture = await bundle(new URL("./fixtures/window-scroll.mjs", import.meta.url).pathname);
const WIDTH = 1200;
const WHEEL_TURNS = 60;
const WHEEL_STEP = 240;

async function wheelTo(evaluate, send, scroller, target, direction) {
  assert.notEqual(await evaluate(`Fixture.lastOf(${JSON.stringify(target)})`), null, `${target} is on the screen`);
  let state = await evaluate(`Fixture.reach(${JSON.stringify(scroller)})`);
  assert.notEqual(state.missing, true, `${scroller} is on the screen`);
  const overflows = state.overflows;
  for (let turn = 0; turn < WHEEL_TURNS && !state.visible; turn += 1) {
    await send("Input.dispatchMouseEvent", { type: "mouseWheel", x: state.x, y: state.y, deltaX: 0, deltaY: direction * WHEEL_STEP });
    await evaluate("Fixture.frames()");
    state = await evaluate(`Fixture.reach(${JSON.stringify(scroller)})`);
  }
  await evaluate("document.querySelector('[data-scroll-target]').removeAttribute('data-scroll-target')");
  return { overflows, visible: state.visible, page: await evaluate("Fixture.pageScroll()") };
}

async function scrollCheck(screen, height, steps) {
  return withPage(async ({ evaluate, send }) => {
    await send("Emulation.setDeviceMetricsOverride", { width: WIDTH, height, deviceScaleFactor: 1, mobile: false });
    await evaluate(fixture);
    await evaluate(`Fixture.mountScreen(${JSON.stringify(screen)})`);
    await evaluate("Fixture.frames()");
    const roots = await evaluate("Fixture.rootStyles()");
    const seen = [];
    for (const [scroller, target, direction] of steps) {
      seen.push({ scroller, target, ...await wheelTo(evaluate, send, scroller, target, direction) });
    }
    return { roots, seen };
  });
}

function assertNativeWindow(screen, height, result, steps) {
  assert.deepEqual(result.seen, steps.map(([scroller, target]) => ({ scroller, target, overflows: true, visible: true, page: [0, 0, 0, 0] })),
    `${screen}: every part is reached through its own scroll and the window never scrolls`);
  const fixed = { overflow: "hidden", overscroll: "none", height };
  assert.deepEqual(result.roots, [fixed, fixed, { ...fixed, overscroll: "auto" }], `${screen}: html, body and the root are fixed to the window`);
}

const DOWN = 1;
const UP = -1;

const cases = [
  ["shell", 600, [[".main", "#last", DOWN], [".main", ".page > p:first-child", UP], [".nav-group", ".nav-group .nav-item", DOWN], [".sidebar", ".sidebar-foot .me", DOWN]]],
  ["launching", 600, [[".main", ".skeleton", DOWN]]],
  ["down", 200, [["main.full-state", "main button", DOWN], ["main.full-state", "main .brand-mark", UP]]],
  ["outdated", 200, [["main.full-state", "main button", DOWN], ["main.full-state", "main .brand-mark", UP]]],
  ["updates", 240, [["main.full-state", "main button", DOWN], ["main.full-state", "main .brand-mark", UP]]],
  ["welcome", 420, [["main.welcome", "main.welcome form button", DOWN], ["main.welcome", "main .brand-mark", UP]]],
];

for (const [screen, height, steps] of cases) {
  test(`the ${screen} screen never scrolls the window and reaches all of its content through its own scroll`, async () => {
    assertNativeWindow(screen, height, await scrollCheck(screen, height, steps), steps);
  });
}

test("on a full-size window the full-window states stay centered and Welcome keeps its centered column", async () => {
  const seen = await withPage(async ({ evaluate, send }) => {
    await send("Emulation.setDeviceMetricsOverride", { width: WIDTH, height: 900, deviceScaleFactor: 1, mobile: false });
    await evaluate(fixture);
    const at = async (screen, selector) => {
      await evaluate(`Fixture.mountScreen(${JSON.stringify(screen)})`);
      await evaluate("Fixture.frames()");
      return evaluate(`Fixture.placement(${JSON.stringify(selector)})`);
    };
    return { down: await at("down", ".full-state .box"), updates: await at("updates", ".full-state .box"), welcome: await at("welcome", ".welcome .intro") };
  });
  assert.ok(Math.abs(seen.down.top - seen.down.bottom) <= 1 && Math.abs(seen.down.left - seen.down.right) <= 1 && seen.down.top > 100, JSON.stringify(seen.down));
  assert.equal(seen.updates.top, 24, "the wide Updates box starts at the top padding");
  assert.ok(Math.abs(seen.updates.left - seen.updates.right) <= 1, JSON.stringify(seen.updates));
  assert.equal(seen.welcome.left, (WIDTH - 960) / 2 + 24, "Welcome's column is centered as before");
  assert.equal(seen.welcome.top, 56);
});
