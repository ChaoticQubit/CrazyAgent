import "./load-source.mjs";
import assert from "node:assert/strict";
import test from "node:test";
import { bundle, withPage } from "./browser.mjs";

const { sendsMessage } = await import("../src/ui/keys.ts");
const { SHORTCUTS } = await import("../src/ui/go-to.tsx");
const fixture = await bundle(new URL("./fixtures/composer-keys.mjs", import.meta.url).pathname);
const SHIFT = 8;
const tick = "new Promise((done) => setTimeout(done, 0))";

const key = (patch = {}) => ({ key: "Enter", shiftKey: false, metaKey: false, ctrlKey: false, altKey: false, isComposing: false, keyCode: 13, ...patch });

test("Enter sends a message, Shift+Enter and input-method Enter never do", () => {
  for (const mac of [true, false]) {
    assert.equal(sendsMessage(key(), mac), true, "Enter sends");
    assert.equal(sendsMessage(key({ shiftKey: true }), mac), false, "Shift+Enter adds a line");
    assert.equal(sendsMessage(key({ isComposing: true }), mac), false, "Enter that picks composed text");
    assert.equal(sendsMessage(key({ keyCode: 229 }), mac), false, "WebKit's composing Enter");
    assert.equal(sendsMessage(key({ altKey: true }), mac), false);
    assert.equal(sendsMessage(key({ key: "a" }), mac), false);
  }
  assert.equal(sendsMessage(key({ metaKey: true }), true), true, "Cmd+Enter still sends on macOS");
  assert.equal(sendsMessage(key({ ctrlKey: true }), true), false);
  assert.equal(sendsMessage(key({ ctrlKey: true }), false), true, "Ctrl+Enter still sends on Windows and Linux");
  assert.equal(sendsMessage(key({ metaKey: true, shiftKey: true }), true), false);
  assert.deepEqual(SHORTCUTS.filter(([keys]) => keys.includes("Enter")), [["Enter", "Send a message or prompt"], ["Shift+Enter", "New line in a message or prompt"]]);
});

async function enter(send, modifiers = 0) {
  for (const type of ["keyDown", "keyUp"]) {
    await send("Input.dispatchKeyEvent", { type, key: "Enter", code: "Enter", windowsVirtualKeyCode: 13, modifiers, ...(type === "keyDown" ? { text: "\r" } : {}) });
  }
}

async function composerKeys(mount) {
  return withPage(async ({ evaluate, send }) => {
    await evaluate(fixture);
    const id = await evaluate(`Fixture.${mount}()`);
    await evaluate(`document.getElementById(${JSON.stringify(id)}).focus()`);
    const read = async () => { await evaluate(tick); return evaluate(`Fixture.field(${JSON.stringify(id)})`); };
    await send("Input.insertText", { text: "first line" });
    await enter(send, SHIFT);
    await send("Input.insertText", { text: "second" });
    const shifted = await read();
    await send("Input.imeSetComposition", { text: "かな", selectionStart: 2, selectionEnd: 2 });
    const composingPrevented = await evaluate(`Fixture.syntheticEnter(${JSON.stringify(id)}, { isComposing: true })`);
    const webkitPrevented = await evaluate(`Fixture.syntheticEnter(${JSON.stringify(id)}, { keyCode: 229 })`);
    await send("Input.insertText", { text: "かな" });
    const composed = await read();
    await enter(send);
    const entered = await read();
    return { shifted, composingPrevented, webkitPrevented, composed, entered };
  });
}

test("in the team terminal composer Enter sends the prompt, Shift+Enter adds a line and an input method's Enter doesn't send", async () => {
  const seen = await composerKeys("mountTerminalComposer");
  assert.equal(seen.shifted.value, "first line\nsecond");
  assert.deepEqual(seen.shifted.sent, []);
  assert.equal(seen.composingPrevented, false);
  assert.equal(seen.webkitPrevented, false);
  assert.deepEqual(seen.composed.sent, [], "nothing is sent while composing");
  assert.equal(seen.composed.value, "first line\nsecondかな");
  assert.deepEqual(seen.entered, { ...seen.entered, value: "", sent: ["first line\nsecondかな"], focused: true });
  assert.match(seen.entered.help, /Enter to send, Shift\+Enter for a new line\./);
});

test("in the master composer Shift+Enter adds a line and Enter is taken as send, never typed", async () => {
  const seen = await composerKeys("mountMasterComposer");
  assert.equal(seen.shifted.value, "first line\nsecond");
  assert.equal(seen.composingPrevented, false);
  assert.equal(seen.webkitPrevented, false);
  assert.equal(seen.composed.value, "first line\nsecondかな");
  assert.equal(seen.entered.value, "first line\nsecondかな", "Enter adds no line; with no engine the send waits");
  assert.equal(seen.entered.help, "Enter to send, Shift+Enter for a new line");
});
