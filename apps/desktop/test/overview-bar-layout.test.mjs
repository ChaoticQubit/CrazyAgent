import assert from "node:assert/strict";
import test from "node:test";
import { bundle, withPage } from "./browser.mjs";

const fixture = await bundle(new URL("./fixtures/overview-bar.mjs", import.meta.url).pathname);
const CONTRAST_MIN = 4.5;
const WINDOWS = [["the default", 1180], ["the narrowest", 760]];

for (const theme of ["light", "dark"]) {
  for (const [name, width] of WINDOWS) {
    test(`in ${theme} at ${name} window the strip stands alone and its status, Edit and Refresh share one row under it`, { timeout: 60_000 }, async () => {
      const seen = await withPage(async (page) => {
        await page.send("Emulation.setDeviceMetricsOverride", { width, height: 800, deviceScaleFactor: 1, mobile: false });
        await page.evaluate(fixture);
        await page.evaluate(`Fixture.mount(${JSON.stringify(theme)})`);
        return {
          ...(await page.evaluate("Fixture.measure()")),
          readable: {
            status: await page.evaluate("Fixture.contrast('.ov-bar [role=status]')"),
            edit: await page.evaluate("Fixture.contrast('#ov-edit-open')"),
          },
        };
      });
      const shown = JSON.stringify(seen);
      assert.deepEqual(seen.buttons.map((button) => button.name), ["Edit", "Refresh"], shown);
      const [edit, refresh] = seen.buttons;
      assert.ok(seen.bar.top >= seen.strip.bottom, `the row is under the strip: ${shown}`);
      assert.deepEqual(seen.borders, ["0px", "0px"], "no rule above or below the row");
      for (const part of [seen.status, edit]) assert.ok(Math.abs(part.middle - refresh.middle) <= 2, `one row: ${shown}`);
      assert.ok(seen.status.right < edit.left && edit.right < refresh.left, `status, then Edit, then Refresh: ${shown}`);
      assert.ok(Math.abs(seen.status.left - seen.bar.left) <= 1, `the status starts the row: ${shown}`);
      assert.ok(Math.abs(refresh.right - seen.bar.right) <= 1, `Refresh ends the row: ${shown}`);
      assert.ok(seen.sideways <= 0, `the page does not scroll sideways: ${seen.sideways} px`);
      assert.ok(seen.readable.status >= CONTRAST_MIN, `status ${seen.readable.status}`);
      assert.ok(seen.readable.edit >= CONTRAST_MIN, `Edit ${seen.readable.edit}`);
    });
  }
}
