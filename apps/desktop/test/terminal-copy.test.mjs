import assert from "node:assert/strict";
import { createServer } from "node:http";
import test from "node:test";
import "./load-source.mjs";
import { bundle, withPage } from "./browser.mjs";

const io = await import("../src/screens/team/terminal-io.ts");
const coord = await import("../src/screens/team/coordination.tsx");

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

async function drag(send, from, to, modifiers = 0) {
  await send("Input.dispatchMouseEvent", { type: "mousePressed", x: from.x, y: from.y, button: "left", buttons: 1, clickCount: 1, modifiers });
  for (const part of [0.25, 0.5, 0.75, 1]) {
    await send("Input.dispatchMouseEvent", { type: "mouseMoved", x: Math.round(from.x + (to.x - from.x) * part), y: from.y, button: "left", buttons: 1, modifiers });
  }
  await send("Input.dispatchMouseEvent", { type: "mouseReleased", x: to.x, y: to.y, button: "left", buttons: 0, clickCount: 1, modifiers });
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

test("the key that forces a drag to select is Option on a Mac and Shift elsewhere, and the terminal's note names it", () => {
  assert.equal(typeof io.dragKey, "function");
  assert.equal(io.dragKey(true), "Option");
  assert.equal(io.dragKey(false), "Shift");
  assert.equal(io.selectNote(true), "Hold Option and drag to select text; the app in this terminal reads the mouse.");
  assert.equal(io.selectNote(false), "Hold Shift and drag to select text; the app in this terminal reads the mouse.");
});

/** What each harness kind's own app writes to its terminal as it starts at each Terminal
 * display, the stand-in for the real app in the copy proof below. Codex's are the modes
 * codex-cli 0.167.1 set in a pty with `tui.alternate_screen` at `never` and at `always`
 * (OB-8): Fullscreen is the alternate screen with mouse reporting on. Claude Code's
 * Fullscreen is its own alternate-screen view with mouse reporting on, its Classic neither.
 * RCLI prints plain lines whatever the display, as it has no view of its own. */
const APP_STARTS = {
  "claude-code": { classic: "\x1b[?2004h", fullscreen: "\x1b[?2004h\x1b[?1049h\x1b[?1000h\x1b[?1002h\x1b[?1003h\x1b[?1006h" },
  codex: { classic: "\x1b[?2004h\x1b[?1004h", fullscreen: "\x1b[?2004h\x1b[?1004h\x1b[?1049h\x1b[?1007l\x1b[?1000h\x1b[?1002h\x1b[?1003h\x1b[?1006h" },
  rcli: { classic: "", fullscreen: "" },
};

const NOTHING_COPIED = "nothing copied";
const MOUSE_REPORT = /\x1b\[<\d+;\d+;\d+[Mm]/;
const READS_MOUSE = /\x1b\[\?100[023]h/;

const harnessPanel = await bundle(new URL("./fixtures/terminal-harnesses.mjs", import.meta.url).pathname);

test("copy works in the master and agent terminal of every harness kind at every Terminal display: a plain drag selects on the normal screen, the forced drag selects while the app reads the mouse and its selection outlives the pointer moving on and a right-click, and the copy key and the menu's Copy put it on the clipboard (OB-8, DEC-141)", async () => {
  const displays = coord.DISPLAY_OPTIONS.map((option) => option.value);
  assert.deepEqual(Object.keys(APP_STARTS), coord.COORD_HARNESSES, "every harness kind has a stand-in, so a new kind cannot skip the copy proof");
  for (const harness of coord.COORD_HARNESSES) assert.deepEqual(Object.keys(APP_STARTS[harness]), displays, `${harness} has a stand-in for every display`);
  let server = null;
  const seen = await withPage(async ({ evaluate, send, context }) => {
    server = await securePage(evaluate, send, context);
    await send("Emulation.setDeviceMetricsOverride", { width: 1000, height: 700, deviceScaleFactor: 1, mobile: false });
    await evaluate(harnessPanel);
    const mac = (await evaluate("navigator.platform")).startsWith("Mac");
    const forced = mac ? 1 : 8;
    const copyKeys = mac ? 4 : 2;
    const runs = [];
    for (const role of ["master", "sub"]) {
      for (const harness of coord.COORD_HARNESSES) {
        for (const display of displays) {
          const name = `${role} ${harness} ${display}`;
          await evaluate(`Fixture.open(${JSON.stringify(role)}, ${JSON.stringify(harness)})`);
          assert.equal(await evaluate("Fixture.until(() => Fixture.view().live)"), true, `${name}: the terminal went live`);
          await evaluate(`Fixture.output(${JSON.stringify(`${APP_STARTS[harness][display]}first line of output\r\nsecond line`)})`);
          assert.equal(await evaluate('Fixture.until(() => Fixture.view().text.includes("second line"))'), true, `${name}: the app's output drew`);
          await evaluate("new Promise((done) => requestAnimationFrame(() => requestAnimationFrame(done)))");
          const at = await evaluate("Fixture.view()");
          const y = Math.round(at.screen.top + at.rowHeight / 2);
          const from = { x: Math.round(at.screen.left + 2), y };
          const to = { x: Math.round(at.screen.left + 140), y };
          const wander = async () => {
            for (const step of [10, 25, 40]) await send("Input.dispatchMouseEvent", { type: "mouseMoved", x: to.x + step, y: y + step });
            await send("Input.dispatchMouseEvent", { type: "mouseMoved", x: to.x, y });
          };
          const copied = async (modifiers) => {
            await evaluate(`navigator.clipboard.writeText(${JSON.stringify(NOTHING_COPIED)})`);
            await drag(send, from, to, modifiers);
            await wander();
            await keyPress(send, copyKeys);
            return evaluate("navigator.clipboard.readText()");
          };
          const plain = await copied(0);
          const reports = (await evaluate("Fixture.view()")).typed.filter((chunk) => MOUSE_REPORT.test(chunk)).length;
          const withKey = await copied(forced);
          await drag(send, from, to, forced);
          await wander();
          await evaluate(`navigator.clipboard.writeText(${JSON.stringify(NOTHING_COPIED)})`);
          await mouse(send, to.x, y, "right");
          assert.equal(await evaluate("Fixture.until(() => Fixture.view().menu.length > 0)"), true, `${name}: the right-click opened the terminal menu`);
          const copy = (await evaluate("Fixture.view()")).menu.find((item) => item.text === "Copy");
          if (copy && !copy.disabled) await mouse(send, Math.round((copy.left + copy.right) / 2), Math.round((copy.top + copy.bottom) / 2), "left");
          else await press(send, "Escape", 27);
          await evaluate("Fixture.until(() => Fixture.view().menu.length === 0)");
          runs.push({ name, harness, display, note: at.note, plain, reports, withKey, menuCopy: copy ? !copy.disabled : null, fromMenu: await evaluate("navigator.clipboard.readText()") });
        }
      }
    }
    return { mac, runs };
  }).finally(() => server?.close());
  assert.equal(seen.runs.length, 2 * coord.COORD_HARNESSES.length * displays.length);
  const reading = seen.runs.filter((run) => READS_MOUSE.test(APP_STARTS[run.harness][run.display]));
  assert.deepEqual(reading.map((run) => run.name), ["master claude-code fullscreen", "master codex fullscreen", "sub claude-code fullscreen", "sub codex fullscreen"], "the full-screen stand-ins read the mouse, as the real apps do");
  for (const run of seen.runs) {
    const dragged = (text) => text !== NOTHING_COPIED && text.trimEnd().length >= 5 && "first line of output".startsWith(text.trimEnd());
    if (reading.includes(run)) {
      assert.equal(run.plain, NOTHING_COPIED, `${run.name}: a plain drag selects nothing while the app reads the mouse, the cause of OB-8`);
      assert.ok(run.reports > 0, `${run.name}: the plain drag went to the app as mouse reports`);
      assert.ok(run.note.endsWith(io.selectNote(seen.mac)), `${run.name}: the status line says how to select, got ${run.note}`);
    } else {
      assert.ok(dragged(run.plain), `${run.name}: a plain drag selected the start of the first row and the copy key copied it, got ${JSON.stringify(run.plain)}`);
      assert.equal(run.reports, 0, `${run.name}: no drag reaches the app on the normal screen`);
      assert.doesNotMatch(run.note, /drag to select/, `${run.name}: nothing to explain when a plain drag selects`);
    }
    assert.ok(dragged(run.withKey), `${run.name}: the forced drag selected the start of the first row, the selection outlived the pointer moving on, and the copy key copied it, got ${JSON.stringify(run.withKey)}`);
    assert.equal(run.menuCopy, true, `${run.name}: the menu's Copy is enabled for the dragged text`);
    assert.equal(run.fromMenu, run.withKey, `${run.name}: the menu's Copy put the same dragged text on the clipboard`);
  }
});
