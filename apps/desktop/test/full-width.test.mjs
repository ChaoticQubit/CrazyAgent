import assert from "node:assert/strict";
import test from "node:test";
import { bundle, withPage } from "./browser.mjs";

const fixture = await bundle(new URL("./fixtures/full-width.mjs", import.meta.url).pathname);
const HEIGHT = 1440;
const NARROW = 1440;
const WIDE = 2560;

async function measure(screen, width) {
  return withPage(async ({ evaluate, send }) => {
    await send("Emulation.setDeviceMetricsOverride", { width, height: HEIGHT, deviceScaleFactor: 1, mobile: false });
    await evaluate(fixture);
    await evaluate(`Fixture.mountScreen(${JSON.stringify(screen)})`);
    await evaluate("Fixture.frames()");
    return evaluate("Fixture.boxes()");
  });
}

for (const screen of ["home", "intake", "new", "settings"]) {
  test(`the ${screen} screen spans the main area to the window's right edge at ${WIDE}x${HEIGHT}`, async () => {
    const boxes = await measure(screen, WIDE);
    assert.equal(boxes.windowWidth, WIDE);
    assert.equal(boxes.mainRight, WIDE, `main reaches the window's right edge: ${JSON.stringify(boxes)}`);
    assert.equal(boxes.pageRight, boxes.mainRight, `the page fills main: ${JSON.stringify(boxes)}`);
    assert.equal(boxes.bodyRight, boxes.pageRight - boxes.pagePadding, `the screen body reaches the page padding: ${JSON.stringify(boxes)}`);
  });
}

test(`the New project and project settings second columns grow with the window from ${NARROW} to ${WIDE}`, async () => {
  for (const screen of ["new", "settings"]) {
    const narrow = await measure(screen, NARROW);
    const wide = await measure(screen, WIDE);
    assert.ok(wide.mainWidth > narrow.mainWidth, screen);
    assert.equal(wide.second - narrow.second, wide.mainWidth - narrow.mainWidth, `${screen}: ${JSON.stringify({ narrow, wide })}`);
  }
});
