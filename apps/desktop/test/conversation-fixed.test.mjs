import assert from "node:assert/strict";
import test from "node:test";
import { bundle, withPage } from "./browser.mjs";

const fixture = await bundle(new URL("./fixtures/conversation-fixed.mjs", import.meta.url).pathname);
const SIZES = [{ width: 1280, height: 800, side: 300 }, { width: 900, height: 640, side: 240 }];
const BOTTOM_PX = 24;

/** Opens the Conversation tab fixture in a window of the given size, lets it settle and
 * hands the page to the check. Inputs: the size, the fixture options and the check. Output:
 * what the check returns. */
function conversation({ width, height }, options, check) {
  return withPage(async ({ evaluate, send }) => {
    await send("Emulation.setDeviceMetricsOverride", { width, height, deviceScaleFactor: 1, mobile: false });
    await evaluate(fixture);
    await evaluate(`Fixture.mount(${JSON.stringify(options)})`);
    await evaluate("Fixture.frames()");
    return check({ evaluate, send });
  });
}

/** Turns the mouse wheel down over an element and lets the page settle. Inputs: the page and
 * the selector. */
async function wheel({ evaluate, send }, selector) {
  const { x, y } = await evaluate(`Fixture.middle(${JSON.stringify(selector)})`);
  for (let turn = 0; turn < 4; turn += 1) {
    await send("Input.dispatchMouseEvent", { type: "mouseWheel", x, y, deltaX: 0, deltaY: 240 });
    await evaluate("Fixture.frames()");
  }
}

/** Asserts the page itself does not scroll: neither the document nor the main region has
 * more to show than its height, and neither has moved. Inputs: the measure and the size. */
function assertFixed(seen, size) {
  const label = `${size.width}x${size.height}`;
  assert.ok(seen.document.scroll <= seen.document.client, `${label}: the document has nothing below the window`);
  assert.ok(seen.main.scroll <= seen.main.client, `${label}: the main region has nothing below the window`);
  assert.deepEqual([seen.document.top, seen.main.top, seen.window.scrollY], [0, 0, 0], `${label}: nothing scrolled`);
}

for (const size of SIZES) {
  test(`at ${size.width}x${size.height} the Conversation tab never scrolls the page, the master reaches the window bottom and the Team bar sits beside it at ${size.side}px (DEC-125)`, { timeout: 60000 }, async () => {
    const seen = await conversation(size, { agents: 30 }, ({ evaluate }) => evaluate("Fixture.measure()"));
    assertFixed(seen, size);
    assert.ok(seen.master.bottom <= size.height && seen.master.bottom >= size.height - BOTTOM_PX, `the master ends within ${BOTTOM_PX}px of the window bottom: ${seen.master.bottom}`);
    assert.ok(seen.side.left >= seen.left.right, "the Team bar is to the right of the master, not under it");
    assert.ok(Math.abs(seen.side.width - size.side) <= 1, `the Team bar is ${size.side}px wide: ${seen.side.width}`);
    assert.ok(Math.abs(seen.side.top - seen.left.top) <= 1 && Math.abs(seen.side.bottom - seen.left.bottom) <= 1, "the Team bar is as tall as the master column");
    assert.ok(Math.abs(seen.master.bottom - seen.left.bottom) <= 1, "the master fills its column");
  });

  test(`at ${size.width}x${size.height} the Team bar with 30 agents scrolls on its own under the wheel while the page stays put`, { timeout: 60000 }, async () => {
    const seen = await conversation(size, { agents: 30 }, async (page) => {
      const before = await page.evaluate("Fixture.measure()");
      await wheel(page, ".ta-scroll");
      const listed = await page.evaluate("Fixture.measure()");
      await wheel(page, ".pj-master-live");
      return { before, listed, after: await page.evaluate("Fixture.measure()") };
    });
    assert.equal(seen.before.rows, 30, "every agent has a row");
    assert.ok(seen.before.team.scroll > seen.before.team.client, "30 agents are more than the Team bar shows at once");
    assert.ok(seen.listed.team.top > 0, "the wheel scrolls the Team bar");
    assertFixed(seen.listed, size);
    assertFixed(seen.after, size);
  });

  test(`at ${size.width}x${size.height} an agent terminal opened from the Team bar fills the right column and the page still does not scroll`, { timeout: 60000 }, async () => {
    const seen = await conversation(size, { agents: 30, terminal: true }, ({ evaluate }) => evaluate("Fixture.measure()"));
    assertFixed(seen, size);
    assert.ok(seen.terminal, "the terminal is in the right column");
    for (const edge of ["top", "bottom", "left", "right"]) {
      assert.ok(Math.abs(seen.terminal[edge] - seen.side[edge]) <= 1, `the terminal's ${edge} is the column's: ${seen.terminal[edge]} vs ${seen.side[edge]}`);
    }
    assert.ok(seen.side.width >= 420 - 1, "the opened terminal keeps its wider split");
    assert.ok(seen.side.left >= seen.left.right, "the terminal is beside the master, not under it");
  });
}

test("in a short window with many banners the tab panel keeps its 240px floor, only the banner box scrolls and the page does not", { timeout: 60000 }, async () => {
  const size = { width: 900, height: 640 };
  const seen = await conversation(size, { agents: 30, banners: 8 }, ({ evaluate }) => evaluate("Fixture.measure()"));
  assertFixed(seen, size);
  assert.ok(seen.panel.height >= 240 - 0.5, `the panel keeps its floor: ${seen.panel.height}`);
  assert.ok(seen.banners.scroll > seen.banners.client, "the banners scroll in their own box");
  assert.ok(seen.panel.bottom <= size.height, "the panel ends inside the window");
});
