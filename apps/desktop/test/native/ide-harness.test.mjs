import assert from "node:assert/strict";
import { test } from "node:test";
import { input, workbench } from "./ide-harness.mjs";

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

test("Enter goes in as a key press that carries its character, so an editor gets the new line, and with a modifier it carries none", async () => {
  const { calls, send } = recorder();
  const keys = input({ send });
  await keys.key("Enter");
  await keys.key("Enter", 4);
  await keys.key("Escape");
  const sent = (type, key, code, windowsVirtualKeyCode, modifiers, typed = {}) => ["Input.dispatchKeyEvent", { type, key, code, windowsVirtualKeyCode, modifiers, ...typed }];
  assert.deepEqual(calls, [
    sent("keyDown", "Enter", "Enter", 13, 0, { text: "\r", unmodifiedText: "\r" }),
    sent("keyUp", "Enter", "Enter", 13, 0),
    sent("rawKeyDown", "Enter", "Enter", 13, 4),
    sent("keyUp", "Enter", "Enter", 13, 4),
    sent("rawKeyDown", "Escape", "Escape", 27, 0),
    sent("keyUp", "Escape", "Escape", 27, 0),
  ]);
});

test("a pick waits for its row while Quick open still searches, then clicks it where it stays", async () => {
  const found = [null, null, { x: 10, y: 20 }, { x: 10, y: 30 }, { x: 10, y: 30 }];
  let reads = 0;
  const page = { evaluate: async (expression) => {
    if (!expression.includes("getBoundingClientRect")) return [];
    reads += 1;
    return found[Math.min(reads, found.length) - 1];
  } };
  const clicks = [];
  const keys = { click: async (box) => { clicks.push(box); } };
  const me = { capture: async () => {} };
  await workbench(page, keys, me).pick("notes.md", "Project");
  assert.deepEqual(clicks, [{ x: 10, y: 30 }]);
  assert.equal(reads, 5, "it read the list until the row stood still");
});
