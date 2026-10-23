import assert from "node:assert/strict";
import { join } from "node:path";

async function clickVisible(button) {
  assert.equal(await button.isDisplayed(), true);
  assert.equal(await button.isEnabled(), true);
  const clicked = await browser.execute((element) => {
    if (!element || element.disabled || element.getClientRects().length === 0) return false;
    element.click();
    return true;
  }, button);
  assert.equal(clicked, true);
}

describe("actual desktop project conversation", () => {
  it("enables the project conversation and sends an explicit owner message", async () => {
    const evidence = process.env.CRAZYAGENT_DESKTOP_TEST_EVIDENCE;
    assert(evidence, "owned native evidence directory required");
    const conversation = await $("button=Conversation");
    assert.equal(await conversation.isDisplayed(), true, "actual Conversation control must be visible");
    assert.equal(await conversation.isEnabled(), true, "actual desktop project conversation must be available");
    await clickVisible(conversation);
    const project = await $('[data-project-id="desktop-project"]');
    await clickVisible(project);
    const editor = await $('[data-testid="conversation-message"]');
    const text = "<script>literal conversation text</script>\nowner message";
    await editor.setValue(text);
    await clickVisible(await $('[data-testid="send-conversation-message"]'));
    const entry = await $('[data-testid="conversation-entry-text"]');
    await browser.waitUntil(async () => await entry.getText() === text, {
      timeout: 10000, timeoutMsg: "actual submitted owner message must appear without clipping",
    });
    assert.equal(await entry.getText(), text);
    assert.equal(await browser.execute(() => document.querySelectorAll(".conversation-history script").length), 0);
    const brief = await $('[data-testid="current-project-brief"]');
    assert.equal(await brief.getText(), "No brief has been recorded.");
    await browser.saveScreenshot(join(evidence, "conversation-message.png"));
  });
});
