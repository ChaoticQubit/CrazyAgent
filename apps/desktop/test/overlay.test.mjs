import assert from "node:assert/strict";
import test from "node:test";
import { bundle, withPage } from "./browser.mjs";

const fixture = await bundle(new URL("./fixtures/overlay.mjs", import.meta.url).pathname);
const tick = "const tick = () => new Promise((done) => setTimeout(done, 0));";

test("a menu item still gets the click when the webview moves focus to main on mousedown, as WKWebView does", async () => {
  const seen = await withPage(async ({ evaluate }) => {
    await evaluate(fixture);
    return evaluate(`(async () => { ${tick}
      Fixture.mountMenu();
      document.querySelector("[aria-haspopup=menu]").click();
      await tick();
      const item = document.querySelector("[role=menuitem]");
      const focused = document.activeElement === item;
      item.dispatchEvent(new PointerEvent("pointerdown", { bubbles: true, composed: true, button: 0 }));
      item.dispatchEvent(new MouseEvent("mousedown", { bubbles: true, button: 0 }));
      document.querySelector("main").focus();
      await tick();
      item.dispatchEvent(new PointerEvent("pointerup", { bubbles: true, composed: true, button: 0 }));
      item.dispatchEvent(new MouseEvent("mouseup", { bubbles: true, button: 0 }));
      item.click();
      await tick();
      return { focused, events: [...Fixture.events], open: document.querySelector("[role=menu]") !== null };
    })()`);
  });
  assert.deepEqual(seen, { focused: true, events: ["delete"], open: false });
});

test("a menu closes on a pointerdown outside it, on focus moving to something outside, and on Esc", async () => {
  const seen = await withPage(async ({ evaluate }) => {
    await evaluate(fixture);
    return evaluate(`(async () => { ${tick}
      const open = () => document.querySelector("[role=menu]") !== null;
      const button = () => document.querySelector("[aria-haspopup=menu]");
      Fixture.mountMenu();
      button().click(); await tick();
      const opened = open();
      document.getElementById("outside").dispatchEvent(new PointerEvent("pointerdown", { bubbles: true, composed: true, button: 0 }));
      await tick();
      const afterOutside = open();
      button().click(); await tick();
      document.getElementById("outside").focus();
      await tick();
      const afterTab = open();
      button().click(); await tick();
      document.querySelector("[role=menuitem]").dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
      await tick();
      return { opened, afterOutside, afterTab, afterEscape: open(), focusBack: document.activeElement === button(), events: [...Fixture.events] };
    })()`);
  });
  assert.deepEqual(seen, { opened: true, afterOutside: false, afterTab: false, afterEscape: false, focusBack: true, events: [] });
});

async function confirmWhile(pending) {
  return withPage(async ({ evaluate, send }) => {
    await evaluate(fixture);
    const box = await evaluate(`(async () => { ${tick}
      Fixture.mountConfirm(${pending});
      await tick();
      const text = document.querySelector("dialog p").getBoundingClientRect();
      return { x: text.left + 4, y: text.top + text.height / 2 };
    })()`);
    for (const type of ["mousePressed", "mouseReleased"]) await send("Input.dispatchMouseEvent", { type, x: box.x, y: box.y, button: "left", clickCount: 1 });
    for (const type of ["keyDown", "keyUp"]) await send("Input.dispatchKeyEvent", { type, key: "Escape", code: "Escape", windowsVirtualKeyCode: 27 });
    return evaluate(`(async () => { ${tick}
      await tick();
      const afterEscape = [...Fixture.events];
      const dialog = document.querySelector("dialog");
      const named = (text) => [...document.querySelectorAll("dialog button")].find((button) => button.textContent === text || button.getAttribute("aria-label") === text);
      const disabled = { keep: named("Keep project")?.disabled ?? null, close: named("Close")?.disabled ?? null };
      named("Keep project")?.click();
      named("Close")?.click();
      await tick();
      return { afterEscape, open: dialog?.open ?? false, disabled, events: [...Fixture.events] };
    })()`);
  });
}

test("a confirm dialog can't be dismissed while its destructive action runs", async () => {
  assert.deepEqual(await confirmWhile(true), {
    afterEscape: [], open: true, disabled: { keep: true, close: true }, events: [],
  });
});

test("a confirm dialog that isn't running answers Esc, its cancel button and Close", async () => {
  const seen = await confirmWhile(false);
  assert.deepEqual(seen.afterEscape, ["cancel"]);
  assert.deepEqual(seen.disabled, { keep: false, close: false });
  assert.deepEqual(seen.events, ["cancel", "cancel", "cancel"]);
});
