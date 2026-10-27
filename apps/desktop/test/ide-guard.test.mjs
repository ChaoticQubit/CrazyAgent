import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { withPage } from "./browser.mjs";

const GUARD = readFileSync(new URL("../src-tauri/src/ide_guard.js", import.meta.url), "utf8");
const ARROWS = [["ArrowLeft", 37, "\u001c"], ["ArrowRight", 39, "\u001d"], ["ArrowUp", 38, "\u001e"], ["ArrowDown", 40, "\u001f"]];
const CONTROL = /[\u0000-\u0008\u000b\u000c\u000e-\u001f]/u;
const tick = "new Promise((done) => setTimeout(done, 0))";

async function arrowThenControl(send, key, code, control) {
  await send("Input.dispatchKeyEvent", { type: "keyDown", key, code: key, windowsVirtualKeyCode: code });
  await send("Input.insertText", { text: control });
  await send("Input.dispatchKeyEvent", { type: "keyUp", key, code: key, windowsVirtualKeyCode: code });
}

async function typeInto(evaluate, send, id) {
  await evaluate(`document.getElementById(${JSON.stringify(id)}).focus()`);
  await send("Input.insertText", { text: "ab" });
  for (const [key, code, control] of ARROWS) await arrowThenControl(send, key, code, control);
  await send("Input.insertText", { text: "c\td" });
  await evaluate(tick);
  return evaluate(`document.getElementById(${JSON.stringify(id)}).value`);
}

const mount = `(() => {
  for (const [id, tag] of [["line", "input"], ["box", "textarea"]]) {
    const field = document.createElement(tag);
    field.id = id;
    document.body.append(field);
  }
})()`;

test("the guard drops control characters an arrow key inserts and keeps typed text", async () => {
  const fields = await withPage(async ({ evaluate, send }) => {
    await evaluate(GUARD);
    await evaluate(mount);
    return { line: await typeInto(evaluate, send, "line"), box: await typeInto(evaluate, send, "box") };
  });
  for (const [id, value] of Object.entries(fields)) {
    assert.doesNotMatch(value, CONTROL, `${id} holds a control character: ${JSON.stringify(value)}`);
    for (const typed of ["a", "b", "c", "d"]) assert.ok(value.includes(typed), `${id} lost ${typed}: ${JSON.stringify(value)}`);
  }
  assert.equal(fields.box, "abc\td", "a tab typed into a text area stays and nothing else moved");
});

test("without the guard the same keys put the control characters in, so the check means something", async () => {
  const value = await withPage(async ({ evaluate, send }) => {
    await evaluate(mount);
    return typeInto(evaluate, send, "box");
  });
  assert.match(value, CONTROL, `the page took no control character: ${JSON.stringify(value)}`);
});
