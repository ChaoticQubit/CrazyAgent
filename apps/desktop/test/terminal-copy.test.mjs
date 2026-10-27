import assert from "node:assert/strict";
import { createServer } from "node:http";
import test from "node:test";
import "./load-source.mjs";
import { bundle, withPage } from "./browser.mjs";

const io = await import("../src/screens/team/terminal-io.ts");

const key = (extra = {}) => ({ type: "keydown", key: "c", shiftKey: false, altKey: false, ctrlKey: false, metaKey: false, ...extra });

test("Cmd+C copies a selection on a Mac, Ctrl+C copies one elsewhere, and Ctrl+C with nothing selected still reaches the agent", () => {
  assert.equal(io.copyKey(key({ metaKey: true }), true, true), true);
  assert.equal(io.copyKey(key({ metaKey: true, key: "C", shiftKey: true }), true, true), true);
  assert.equal(io.copyKey(key({ type: "keyup", metaKey: true }), true, true), true, "every event of the copy key stays out of xterm");
  assert.equal(io.copyKey(key({ metaKey: true }), false, true), false, "nothing selected, nothing to copy");
  assert.equal(io.copyKey(key({ ctrlKey: true }), true, true), false, "Ctrl+C on a Mac always interrupts the agent");
  assert.equal(io.copyKey(key({ ctrlKey: true }), true, false), true);
  assert.equal(io.copyKey(key({ ctrlKey: true, shiftKey: true }), true, false), true);
  assert.equal(io.copyKey(key({ ctrlKey: true }), false, false), false, "Ctrl+C with nothing selected interrupts the agent");
  assert.equal(io.copyKey(key({ metaKey: true, altKey: true }), true, true), false);
  assert.equal(io.copyKey(key({ metaKey: true, ctrlKey: true }), true, true), false);
  assert.equal(io.copyKey(key({ metaKey: true, key: "v" }), true, true), false);
});

test("Copy output keeps every written line and drops trailing spaces and the blank rows under the last line", () => {
  assert.equal(io.outputText(["one", "two  ", "", "three", "", "   "]), "one\ntwo\n\nthree");
  assert.equal(io.outputText(["", ""]), "");
  assert.equal(io.outputText([]), "");
});

const fixture = await bundle(new URL("./fixtures/live-master.mjs", import.meta.url).pathname);

async function keyPress(send, modifiers) {
  const event = { modifiers, key: "c", code: "KeyC", windowsVirtualKeyCode: 67, nativeVirtualKeyCode: 67 };
  await send("Input.dispatchKeyEvent", { type: "rawKeyDown", ...event });
  if (modifiers === 0) await send("Input.dispatchKeyEvent", { type: "char", text: "c", ...event });
  await send("Input.dispatchKeyEvent", { type: "keyUp", ...event });
}

async function securePage(evaluate, send) {
  const server = createServer((_, reply) => reply.end("<!doctype html><title>terminal</title>"));
  await new Promise((done) => server.listen(0, "127.0.0.1", done));
  const origin = `http://127.0.0.1:${server.address().port}`;
  await send("Browser.grantPermissions", { origin, permissions: ["clipboardReadWrite", "clipboardSanitizedWrite"] });
  await send("Page.navigate", { url: `${origin}/` });
  for (let waited = 0; !(await evaluate("location.origin + document.readyState")).endsWith("complete") || await evaluate("location.origin") !== origin; waited += 20) {
    assert.ok(waited < 5000, "the page never loaded");
    await new Promise((done) => setTimeout(done, 20));
  }
  return server;
}

test("in a real terminal, Cmd+C puts the selection on the clipboard and types nothing, and Ctrl+C with nothing selected interrupts the agent", async () => {
  let server = null;
  const seen = await withPage(async ({ evaluate, send }) => {
    server = await securePage(evaluate, send);
    await evaluate(fixture);
    await evaluate("Fixture.mountKeys()");
    await evaluate(`Fixture.write(${JSON.stringify("first line of output\r\nsecond line")})`);
    await evaluate("Fixture.selectAll()");
    const selection = await evaluate("Fixture.selection()");
    await keyPress(send, 4);
    const copied = await evaluate("navigator.clipboard.readText()");
    const typedAfterCopy = await evaluate("Fixture.typedText()");
    await evaluate("Fixture.clearSelection()");
    await keyPress(send, 2);
    return { selection, copied, typedAfterCopy, typedAfterInterrupt: await evaluate("Fixture.typedText()") };
  }).finally(() => server?.close());
  assert.match(seen.selection, /^first line of output\nsecond line\n/);
  assert.equal(seen.copied, seen.selection, "the clipboard holds exactly the selection");
  assert.deepEqual(seen.typedAfterCopy, [], "the copy key types nothing into the agent");
  assert.deepEqual(seen.typedAfterInterrupt, ["\x03"]);
});
