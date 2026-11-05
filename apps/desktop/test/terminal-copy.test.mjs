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

test("each master input choice says whether keys reach the terminal and whether the message box shows", () => {
  assert.deepEqual(io.inputMode("terminal"), { keys: true, box: false });
  assert.deepEqual(io.inputMode("box"), { keys: false, box: true });
  assert.deepEqual(io.inputMode("both"), { keys: true, box: true });
  assert.deepEqual(io.INPUT_CHOICES.map((choice) => choice.value), ["terminal", "box", "both"]);
  assert.equal(io.INPUT_DEFAULT, "terminal");
  assert.equal(io.inputChoice("box"), "box");
  assert.equal(io.inputChoice("something else"), "terminal", "an unknown stored value reads as the default");
  assert.equal(io.inputChoice(null), "terminal");
});

const fixture = await bundle(new URL("./fixtures/live-master.mjs", import.meta.url).pathname);

async function keyPress(send, modifiers) {
  const event = { modifiers, key: "c", code: "KeyC", windowsVirtualKeyCode: 67, nativeVirtualKeyCode: 67 };
  await send("Input.dispatchKeyEvent", { type: "rawKeyDown", ...event });
  if (modifiers === 0) await send("Input.dispatchKeyEvent", { type: "char", text: "c", ...event });
  await send("Input.dispatchKeyEvent", { type: "keyUp", ...event });
}

async function securePage(evaluate, send, context) {
  const server = createServer((_, reply) => reply.end("<!doctype html><title>terminal</title>"));
  await new Promise((done) => server.listen(0, "127.0.0.1", done));
  const origin = `http://127.0.0.1:${server.address().port}`;
  await send("Browser.grantPermissions", { origin, permissions: ["clipboardReadWrite", "clipboardSanitizedWrite"], browserContextId: context });
  await send("Page.navigate", { url: `${origin}/` });
  for (let waited = 0; !(await evaluate("location.origin + document.readyState")).endsWith("complete") || await evaluate("location.origin") !== origin; waited += 20) {
    assert.ok(waited < 5000, "the page never loaded");
    await new Promise((done) => setTimeout(done, 20));
  }
  return server;
}

test("in a real terminal, Cmd+C puts the selection on the clipboard and types nothing, and Ctrl+C with nothing selected interrupts the agent", async () => {
  let server = null;
  const seen = await withPage(async ({ evaluate, send, context }) => {
    server = await securePage(evaluate, send, context);
    await evaluate(fixture);
    await evaluate("Fixture.mountKeys(true)");
    await evaluate(`Fixture.write(${JSON.stringify("first line of output\r\nsecond line")})`);
    await evaluate("Fixture.selectAll()");
    const selection = await evaluate("Fixture.selection()");
    await keyPress(send, 4);
    const copied = await evaluate("navigator.clipboard.readText()");
    const typedAfterCopy = await evaluate("Fixture.typedText()");
    const kept = await evaluate("Fixture.selection()");
    await evaluate("Fixture.clearSelection()");
    await keyPress(send, 2);
    return { selection, copied, typedAfterCopy, kept, typedAfterInterrupt: await evaluate("Fixture.typedText()") };
  }).finally(() => server?.close());
  assert.match(seen.selection, /^first line of output\nsecond line\n/);
  assert.equal(seen.copied, seen.selection, "the clipboard holds exactly the selection");
  assert.deepEqual(seen.typedAfterCopy, [], "the copy key types nothing into the agent");
  assert.equal(seen.kept, seen.selection, "on a Mac the selection stays after Cmd+C, as in Terminal");
  assert.deepEqual(seen.typedAfterInterrupt, ["\x03"]);
});

test("off a Mac, Ctrl+C copies the selection and lets it go, so the next Ctrl+C interrupts the agent", async () => {
  let server = null;
  const seen = await withPage(async ({ evaluate, send, context }) => {
    server = await securePage(evaluate, send, context);
    await evaluate(fixture);
    await evaluate("Fixture.mountKeys(false)");
    await evaluate(`Fixture.write(${JSON.stringify("first line of output\r\nsecond line")})`);
    await evaluate("Fixture.selectAll()");
    const selection = await evaluate("Fixture.selection()");
    await keyPress(send, 2);
    const copied = await evaluate("navigator.clipboard.readText()");
    const afterCopy = { selection: await evaluate("Fixture.selection()"), typed: await evaluate("Fixture.typedText()") };
    await keyPress(send, 2);
    return { selection, copied, afterCopy, typedAfterSecond: await evaluate("Fixture.typedText()") };
  }).finally(() => server?.close());
  assert.match(seen.selection, /^first line of output\nsecond line\n/);
  assert.equal(seen.copied, seen.selection, "the clipboard holds exactly the selection");
  assert.deepEqual(seen.afterCopy, { selection: "", typed: [] }, "the copy types nothing and lets the selection go");
  assert.deepEqual(seen.typedAfterSecond, ["\x03"], "the next Ctrl+C reaches the agent");
});

test("the terminal menu opens at the click and flips up or left near an edge, never outside its panel", () => {
  const menu = { width: 180, height: 70 };
  const panel = { width: 600, height: 300 };
  assert.deepEqual(io.menuSpot({ x: 10, y: 10 }, menu, panel), { x: 10, y: 10 });
  assert.deepEqual(io.menuSpot({ x: 590, y: 290 }, menu, panel), { x: 410, y: 220 });
  assert.deepEqual(io.menuSpot({ x: 500, y: 100 }, menu, panel), { x: 320, y: 100 });
  assert.deepEqual(io.menuSpot({ x: 20, y: 280 }, menu, panel), { x: 20, y: 210 });
  assert.deepEqual(io.menuSpot({ x: 100, y: 50 }, menu, { width: 150, height: 60 }), { x: 0, y: 0 }, "a panel smaller than the menu pins it to the corner");
});

async function mouse(send, x, y, button, clickCount = 1) {
  await send("Input.dispatchMouseEvent", { type: "mousePressed", x, y, button, clickCount });
  await send("Input.dispatchMouseEvent", { type: "mouseReleased", x, y, button, clickCount });
}

async function press(send, name, code) {
  await send("Input.dispatchKeyEvent", { type: "rawKeyDown", key: name, code: name, windowsVirtualKeyCode: code, nativeVirtualKeyCode: code });
  await send("Input.dispatchKeyEvent", { type: "keyUp", key: name, code: name, windowsVirtualKeyCode: code, nativeVirtualKeyCode: code });
}

async function settledMenu(evaluate) {
  await evaluate("new Promise((done) => requestAnimationFrame(() => requestAnimationFrame(done)))");
  return evaluate("Fixture.menuView()");
}

const inside = (inner, outer) => inner.left >= outer.left && inner.top >= outer.top && inner.right <= outer.right && inner.bottom <= outer.bottom;

test("a right-click on the terminal's last rows or near its right edge opens the whole menu inside the panel", async () => {
  let server = null;
  const seen = await withPage(async ({ evaluate, send, context }) => {
    server = await securePage(evaluate, send, context);
    await send("Emulation.setDeviceMetricsOverride", { width: 1000, height: 700, deviceScaleFactor: 1, mobile: false });
    await evaluate(fixture);
    await evaluate("Fixture.mountMenu(true)");
    await evaluate(`Fixture.write(${JSON.stringify("first line of output\r\nsecond line")})`);
    const start = await settledMenu(evaluate);
    await mouse(send, start.screen.left + 12, start.screen.top + 40, "right");
    const near = await settledMenu(evaluate);
    await press(send, "Escape", 27);
    await mouse(send, start.screen.right - 4, start.screen.bottom - 4, "right");
    return { start, near, corner: await settledMenu(evaluate) };
  }).finally(() => server?.close());
  assert.equal(seen.corner.open, true);
  assert.ok(inside(seen.corner.menu, seen.corner.panel), `menu ${JSON.stringify(seen.corner.menu)} inside panel ${JSON.stringify(seen.corner.panel)}`);
  assert.equal(seen.near.open, true);
  assert.deepEqual([seen.near.menu.left, seen.near.menu.top], [seen.start.screen.left + 12, seen.start.screen.top + 40], "with room, the menu opens at the click");
});

test("the terminal menu moves with the arrow keys and Escape hands focus back to the terminal", async () => {
  let server = null;
  const seen = await withPage(async ({ evaluate, send, context }) => {
    server = await securePage(evaluate, send, context);
    await evaluate(fixture);
    await evaluate("Fixture.mountMenu(true)");
    await evaluate(`Fixture.write(${JSON.stringify("first line of output\r\nsecond line")})`);
    await evaluate("Fixture.selectAll()");
    const start = await settledMenu(evaluate);
    await mouse(send, start.screen.left + 40, start.screen.top + 8, "right");
    const focus = [(await settledMenu(evaluate)).focus];
    for (const [name, code] of [["ArrowDown", 40], ["ArrowDown", 40], ["ArrowUp", 38]]) {
      await press(send, name, code);
      focus.push((await settledMenu(evaluate)).focus);
    }
    await press(send, "Escape", 27);
    return { focus, closed: await settledMenu(evaluate) };
  }).finally(() => server?.close());
  assert.deepEqual(seen.focus, ["Copy", "Copy output", "Copy", "Copy output"]);
  assert.deepEqual([seen.closed.open, seen.closed.focus, seen.closed.notes], [false, "terminal", [null]]);
});

test("Copy output in the terminal menu puts the whole output on the clipboard and hands focus back to the terminal", async () => {
  let server = null;
  const seen = await withPage(async ({ evaluate, send, context }) => {
    server = await securePage(evaluate, send, context);
    await evaluate(fixture);
    await evaluate("Fixture.mountMenu(true)");
    await evaluate(`Fixture.write(${JSON.stringify("first line of output\r\nsecond line")})`);
    await evaluate("navigator.clipboard.writeText('before')");
    const start = await settledMenu(evaluate);
    await mouse(send, start.screen.left + 300, start.screen.top + 120, "right");
    const open = await settledMenu(evaluate);
    const item = open.items.find((entry) => entry.text === "Copy output");
    await mouse(send, Math.round((item.left + item.right) / 2), Math.round((item.top + item.bottom) / 2), "left");
    for (let waited = 0; (await evaluate("Fixture.menuView()")).open && waited < 2000; waited += 20) await new Promise((done) => setTimeout(done, 20));
    return { open, closed: await settledMenu(evaluate), clipboard: await evaluate("navigator.clipboard.readText()") };
  }).finally(() => server?.close());
  assert.equal(seen.open.open, true);
  assert.equal(seen.clipboard, "first line of output\nsecond line");
  assert.deepEqual([seen.closed.open, seen.closed.focus, seen.closed.notes], [false, "terminal", ["Copied the output."]]);
});

async function drag(send, from, to) {
  await send("Input.dispatchMouseEvent", { type: "mousePressed", x: from.x, y: from.y, button: "left", buttons: 1, clickCount: 1 });
  for (const part of [0.25, 0.5, 0.75, 1]) {
    await send("Input.dispatchMouseEvent", { type: "mouseMoved", x: Math.round(from.x + (to.x - from.x) * part), y: from.y, button: "left", buttons: 1 });
  }
  await send("Input.dispatchMouseEvent", { type: "mouseReleased", x: to.x, y: to.y, button: "left", buttons: 0, clickCount: 1 });
}

test("the panel's xterm never lets a right-click replace the selection with the word under the pointer (N-D9)", async () => {
  const seen = await withPage(async ({ evaluate }) => {
    await evaluate(fixture);
    return evaluate("Fixture.rightClickSelectsWord()");
  });
  assert.equal(seen, false, "terminalOptions sets rightClickSelectsWord to false, as xterm turns it on by default on a Mac");
});

test("a drag then a right-click where the button was let go keeps the dragged text, so the menu's Copy puts exactly it on the clipboard (N-D9)", async () => {
  let server = null;
  const seen = await withPage(async ({ evaluate, send, context }) => {
    server = await securePage(evaluate, send, context);
    await send("Emulation.setDeviceMetricsOverride", { width: 1000, height: 700, deviceScaleFactor: 1, mobile: false });
    await evaluate(fixture);
    await evaluate("Fixture.mountMenu(true)");
    await evaluate(`Fixture.write(${JSON.stringify("first line of output\r\nsecond line")})`);
    const start = await settledMenu(evaluate);
    const cell = { width: (start.screen.right - start.screen.left) / 80, height: (start.screen.bottom - start.screen.top) / 10 };
    const y = Math.round(start.screen.top + cell.height / 2);
    const from = { x: start.screen.left + 2, y };
    const runs = [];
    for (const [where, x] of [["mid-text", Math.round(start.screen.left + cell.width * 9.4)], ["right edge", start.screen.right - 2]]) {
      await evaluate("Fixture.clearSelection()");
      await evaluate("navigator.clipboard.writeText('before')");
      await drag(send, from, { x, y });
      const dragged = await evaluate("Fixture.selection()");
      await mouse(send, x, y, "right");
      const open = await settledMenu(evaluate);
      const copy = open.items.find((entry) => entry.text === "Copy");
      if (copy && !copy.disabled) await mouse(send, Math.round((copy.left + copy.right) / 2), Math.round((copy.top + copy.bottom) / 2), "left");
      else await press(send, "Escape", 27);
      for (let waited = 0; (await evaluate("Fixture.menuView()")).open && waited < 2000; waited += 20) await new Promise((done) => setTimeout(done, 20));
      const closed = await settledMenu(evaluate);
      runs.push({ where, dragged, open: open.open, copy: copy ? { disabled: copy.disabled } : null, clipboard: await evaluate("navigator.clipboard.readText()"), note: closed.notes.at(-1) });
    }
    return runs;
  }).finally(() => server?.close());
  for (const run of seen) {
    assert.match(run.dragged, /^first lin/, `${run.where}: the drag selected the start of the first row`);
    assert.equal(run.open, true, `${run.where}: the right-click opened the terminal menu`);
    assert.deepEqual(run.copy, { disabled: false }, `${run.where}: Copy is enabled for the dragged text`);
    assert.equal(run.clipboard, run.dragged, `${run.where}: the clipboard holds exactly the dragged text, not the word under the pointer`);
    assert.equal(run.note, "Copied the selection.", `${run.where}: the terminal says the copy worked`);
  }
});
