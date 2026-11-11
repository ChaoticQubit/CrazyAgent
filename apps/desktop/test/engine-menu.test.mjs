import "./load-source.mjs";
import assert from "node:assert/strict";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { bundle, withPage } from "./browser.mjs";

const shell = await import("../src/ui/shell.tsx");
const fixture = await bundle(new URL("./fixtures/engine-menu.mjs", import.meta.url).pathname);
const FOOT = ".sidebar-foot";
const ITEM = ".sidebar-foot [role=menuitem]";
const TABS_MAX = 12;
const WORDS = "This stops the live master and every running agent. The engine comes back in a few seconds. Your projects, conversations and settings are kept.";
const forbidden = () => { throw new Error("static rendering must not restart the engine"); };

/** Runs steps on a page showing the sidebar. Inputs: the restart control's state and the
 * steps, given the page helpers. Output: what the steps return. */
function onSidebar(options, steps) {
  return withPage(async ({ evaluate, send }) => {
    await send("Emulation.setDeviceMetricsOverride", { width: 1200, height: 800, deviceScaleFactor: 1, mobile: false });
    await evaluate(fixture);
    await evaluate(`Fixture.mount(${JSON.stringify(options)})`);
    const shown = () => evaluate("new Promise((done) => setTimeout(() => done(Fixture.shown()), 60))");
    const point = async (selector) => {
      const at = await evaluate(`Fixture.centerOf(${JSON.stringify(selector)})`);
      assert.ok(at, `nothing at ${selector}`);
      return at;
    };
    const hover = async (selector) => send("Input.dispatchMouseEvent", { type: "mouseMoved", ...(await point(selector)) });
    const away = () => send("Input.dispatchMouseEvent", { type: "mouseMoved", x: 800, y: 200 });
    const click = async (selector) => {
      const at = await point(selector);
      await send("Input.dispatchMouseEvent", { type: "mouseMoved", ...at });
      for (const type of ["mousePressed", "mouseReleased"]) await send("Input.dispatchMouseEvent", { type, ...at, button: "left", clickCount: 1 });
    };
    const key = async (name, code, text = undefined) => {
      await send("Input.dispatchKeyEvent", { type: "keyDown", key: name, code: name, windowsVirtualKeyCode: code, text });
      await send("Input.dispatchKeyEvent", { type: "keyUp", key: name, code: name, windowsVirtualKeyCode: code });
    };
    const tabTo = async (focus) => {
      for (let press = 0; press < TABS_MAX; press += 1) {
        await key("Tab", 9);
        if ((await shown()).focus === focus) return;
      }
      assert.fail(`Tab never reached ${focus}`);
    };
    const update = (next) => evaluate(`Fixture.update(${JSON.stringify(next)})`);
    return steps({ shown, hover, away, click, key, tabTo, update });
  });
}

test("hovering the bottom-left block opens a menu with Restart engine, and moving away closes it", async () => {
  await onSidebar({}, async ({ shown, hover, away }) => {
    assert.equal((await shown()).menu, false, "no menu before the pointer comes");
    await hover(FOOT);
    const open = await shown();
    assert.equal(open.menu, true);
    assert.deepEqual(open.items, ["Restart engine"]);
    assert.equal(open.disabled, false);
    await hover(ITEM);
    assert.equal((await shown()).menu, true, "the menu stays while the pointer is on it");
    await away();
    assert.equal((await shown()).menu, false);
  });
});

test("keyboard focus opens the same menu, Tab reaches Restart engine, and Esc closes it with focus kept in the block", async () => {
  await onSidebar({}, async ({ shown, key, tabTo }) => {
    assert.equal((await shown()).menu, false, "no menu before focus comes");
    await tabTo("me");
    assert.equal((await shown()).menu, true, "focus on the profile name opens the menu");
    await key("Tab", 9);
    assert.equal((await shown()).focus, "menuitem");
    await key("Escape", 27);
    const closed = await shown();
    assert.equal(closed.menu, false);
    assert.equal(closed.focus, "me", "focus goes back to the profile name, not to nothing");
    assert.deepEqual(closed.events, []);
  });
});

test("Esc on the hovered menu closes it until the pointer leaves and comes back", async () => {
  await onSidebar({}, async ({ shown, hover, away, key }) => {
    await hover(FOOT);
    assert.equal((await shown()).menu, true);
    await key("Escape", 27);
    const closed = await shown();
    assert.equal(closed.menu, false);
    assert.equal(closed.focus, "body", "focus outside the block is left alone");
    await away();
    await hover(FOOT);
    assert.equal((await shown()).menu, true);
  });
});

test("Restart engine asks first and says what stops and what is kept", async () => {
  await onSidebar({}, async ({ shown, hover, click }) => {
    await hover(FOOT);
    await click(ITEM);
    const asked = await shown();
    assert.deepEqual(asked.dialog, { title: "Restart the engine?", words: WORDS, buttons: ["Cancel", "Restart engine"] });
    assert.deepEqual(asked.events, [], "nothing restarts before the owner confirms");
  });
});

test("Cancel does nothing", async () => {
  await onSidebar({}, async ({ shown, hover, click }) => {
    await hover(FOOT);
    await click(ITEM);
    await click("dialog[open] .dialog-foot button:first-child");
    const after = await shown();
    assert.equal(after.dialog, null);
    assert.deepEqual(after.events, []);
    assert.equal(after.status, "Engine running");
  });
});

test("after Cancel the menu is closed and focus rests on the profile name", async () => {
  await onSidebar({}, async ({ shown, key, tabTo }) => {
    await tabTo("menuitem");
    await key("Enter", 13, "\r");
    assert.equal((await shown()).dialog?.title, "Restart the engine?");
    await key("Enter", 13, "\r");
    const after = await shown();
    assert.equal(after.dialog, null, "Enter on the focused Cancel closes the question");
    assert.equal(after.menu, false);
    assert.equal(after.focus, "me");
    assert.deepEqual(after.events, []);
  });
});

test("an update install that starts while the question is open stops the restart", async () => {
  await onSidebar({}, async ({ shown, hover, click, update }) => {
    await hover(FOOT);
    await click(ITEM);
    await update({ held: true });
    await click("dialog[open] .dialog-foot button:last-child");
    const after = await shown();
    assert.equal(after.dialog, null);
    assert.deepEqual(after.events, [], "the held restart never ran");
  });
});

test("Esc closes the confirm dialog without restarting", async () => {
  await onSidebar({}, async ({ shown, hover, click, key }) => {
    await hover(FOOT);
    await click(ITEM);
    await key("Escape", 27);
    const after = await shown();
    assert.equal(after.dialog, null);
    assert.deepEqual(after.events, []);
  });
});

test("Confirm restarts the engine once", async () => {
  await onSidebar({}, async ({ shown, hover, click }) => {
    await hover(FOOT);
    await click(ITEM);
    await click("dialog[open] .dialog-foot button:last-child");
    const after = await shown();
    assert.equal(after.dialog, null);
    assert.deepEqual(after.events, ["restart"]);
  });
});

test("Restart engine is off while an update install holds it, and says why", async () => {
  await onSidebar({ held: true }, async ({ shown, hover, click }) => {
    await hover(FOOT);
    const held = await shown();
    assert.deepEqual(held.items, ["Restart engine"]);
    assert.equal(held.disabled, true);
    assert.equal(held.note, "Waits until the update install ends.");
    await click(ITEM);
    const after = await shown();
    assert.equal(after.dialog, null);
    assert.deepEqual(after.events, []);
  });
});

test("while the engine restarts the block says so and Restart engine is off", async () => {
  await onSidebar({ busy: true }, async ({ shown, hover }) => {
    await hover(FOOT);
    const busy = await shown();
    assert.equal(busy.status, "Restarting engine...");
    assert.equal(busy.disabled, true);
    assert.equal(busy.note, null);
  });
});

test("a failed restart shows its plain words in the block and Restart engine stays on", async () => {
  await onSidebar({ failure: "The engine did not start again." }, async ({ shown, hover }) => {
    await hover(FOOT);
    const failed = await shown();
    assert.equal(failed.alert, "The engine did not start again.");
    assert.equal(failed.disabled, false);
  });
});

test("at rest the block draws no menu and no dialog", () => {
  const restart = { held: false, busy: false, failure: "", onRestart: forbidden };
  const markup = renderToStaticMarkup(createElement(shell.Sidebar, { route: { name: "home" }, projects: [], profileName: "Mira Okafor", engine: "running", onRetry: forbidden, restart }));
  assert.doesNotMatch(markup, /role="menu"|<dialog|Restart engine/);
  assert.match(markup, /Engine running/);
});

test("a footer without a profile name is a programming error", async () => {
  const foot = await import("../src/ui/sidebar-foot.tsx");
  assert.throws(() => renderToStaticMarkup(createElement(foot.SidebarFoot, { name: "", engine: "running", onRetry: forbidden })), /the footer names a profile/);
});

test("without a restart control the block keeps its words and has no menu", () => {
  const markup = renderToStaticMarkup(createElement(shell.Sidebar, { route: { name: "home" }, projects: [], profileName: "", engine: "down", onRetry: forbidden }));
  assert.match(markup, />Profile<\/a>/);
  assert.match(markup, /Try again/);
  assert.match(markup, /Engine not running/);
});
