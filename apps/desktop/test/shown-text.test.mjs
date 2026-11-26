import assert from "node:assert/strict";
import test from "node:test";
import { withPage } from "./browser.mjs";
import { shownText } from "./native/shown-text.mjs";

const POPUP = "section.qp-popup";

/** Puts one pop-up in the page body. Inputs: the page and the markup. Output: nothing. */
async function show(page, markup) {
  await page.evaluate(`document.body.innerHTML = ${JSON.stringify(markup)};`);
}

test("the questions flow reads a pop-up's text only while the owner can see it", async () => {
  await withPage(async (page) => {
    await show(page, `<section class="qp-popup">A question for you</section>`);
    assert.equal(await page.evaluate(shownText(POPUP)), "A question for you");
    await show(page, `<section class="qp-popup" hidden>A question for you</section>`);
    assert.equal(await page.evaluate(shownText(POPUP)), "");
    await show(page, `<div style="display: none"><section class="qp-popup">A question for you</section></div>`);
    assert.equal(await page.evaluate(shownText(POPUP)), "");
    await show(page, `<section class="qp-popup" style="width: 0; height: 0; overflow: hidden; padding: 0; border: 0">A question for you</section>`);
    assert.equal(await page.evaluate(shownText(POPUP)), "");
    await show(page, "");
    assert.equal(await page.evaluate(shownText(POPUP)), "");
  });
});
