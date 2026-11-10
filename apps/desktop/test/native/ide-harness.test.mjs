import assert from "node:assert/strict";
import { test } from "node:test";
import { input } from "./ide-harness.mjs";

/** Records each DevTools call the harness makes. Output: the calls and a send that keeps them. */
function recorder() {
  const calls = [];
  return { calls, send: async (method, params) => { calls.push([method, params]); } };
}

test("typed text goes in as one key press per character, the way a keyboard types it (IDE-TERM-FOCUS-TYPING)", async () => {
  const { calls, send } = recorder();
  await input({ send }).text("a {");
  assert.deepEqual(calls, ["a", " ", "{"].flatMap((ch) => [
    ["Input.dispatchKeyEvent", { type: "keyDown", key: ch, text: ch, unmodifiedText: ch }],
    ["Input.dispatchKeyEvent", { type: "keyUp", key: ch }],
  ]));
  assert.ok(calls.every(([method]) => method !== "Input.insertText"), "no text arrives without its keys");
});

test("a control character is refused, so Enter and Tab go through key()", async () => {
  const { calls, send } = recorder();
  await assert.rejects(input({ send }).text("ls\n"), /type control characters with key\(\)/);
  assert.deepEqual(calls, []);
});
