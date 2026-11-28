import assert from "node:assert/strict";
import test from "node:test";
import { withPage } from "./browser.mjs";
import { shownField, shownText, terminalSettled } from "./native/shown-text.mjs";

const POPUP = "section.qp-popup";
const PANEL = "#pj-tab-panel";
const FIELD = "#pj-tab-panel .xterm-helper-textarea, #pj-tab-panel textarea";

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

test("the questions flow waits for the tab's terminal to settle before it picks a field", async () => {
  await withPage(async (page) => {
    await show(page, `<div id="pj-tab-panel"><p class="tg-terminal-state" role="status"><svg class="icon spin"></svg>Connecting</p><div class="tg-terminal-screen"><textarea class="xterm-helper-textarea"></textarea></div></div>`);
    assert.equal(await page.evaluate(terminalSettled(PANEL)), false);
    await show(page, `<div id="pj-tab-panel"><section>Agent terminals isn't ready yet</section><div class="tg-terminal-screen" hidden><textarea class="xterm-helper-textarea"></textarea></div></div>`);
    assert.equal(await page.evaluate(terminalSettled(PANEL)), true);
    await show(page, `<div id="pj-tab-panel"><div class="tg-terminal-screen"><textarea class="xterm-helper-textarea"></textarea></div><div class="tg-terminal-note">Live</div></div>`);
    assert.equal(await page.evaluate(terminalSettled(PANEL)), true);
    await show(page, `<div id="pj-tab-panel"><p class="tg-terminal-state" role="status">This agent has no terminal yet.</p></div>`);
    assert.equal(await page.evaluate(terminalSettled(PANEL)), true);
  });
});

test("the questions flow puts the keyboard only in a field the owner can see", async () => {
  await withPage(async (page) => {
    await show(page, `<div id="pj-tab-panel"><div class="tg-terminal-screen" hidden><textarea class="xterm-helper-textarea"></textarea></div><textarea class="composer"></textarea></div>`);
    assert.equal(await page.evaluate(shownField(FIELD)), "composer");
    assert.equal(await page.evaluate("document.activeElement.className"), "composer");
    await show(page, `<div id="pj-tab-panel"><div class="tg-terminal-screen" hidden><textarea class="xterm-helper-textarea"></textarea></div></div>`);
    assert.equal(await page.evaluate(shownField(FIELD)), null);
    await show(page, `<div id="pj-tab-panel"><div class="tg-terminal-screen"><textarea class="xterm-helper-textarea"></textarea></div></div>`);
    assert.equal(await page.evaluate(shownField(FIELD)), "xterm-helper-textarea");
    for (const style of ["visibility: hidden", "opacity: 0"]) {
      await show(page, `<div id="pj-tab-panel"><div class="tg-terminal-screen" style="${style}"><textarea class="xterm-helper-textarea"></textarea></div><textarea class="composer"></textarea></div>`);
      assert.equal(await page.evaluate(shownField(FIELD)), "composer", style);
    }
  });
});
