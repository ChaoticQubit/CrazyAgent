import "./load-source.mjs";
import assert from "node:assert/strict";
import test from "node:test";
import { setTimeout as pause } from "node:timers/promises";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { bundle, withPage } from "./browser.mjs";

const shell = await import("../src/ui/shell.tsx");
const fixture = await bundle(new URL("./fixtures/engine-menu.mjs", import.meta.url).pathname);
const FOOT = ".sidebar-foot";
const ITEM = ".sidebar-foot [data-item=restart]";
const NAME = ".sidebar-foot .me";
const USER_ITEMS = [["profile", "Profile"], ["updates", "Updates"], ["github", "GitHub"], ["home", "Projects folder"]];
const ALL = [...USER_ITEMS.map(([, label]) => label), "Restart engine"];
const TABS_MAX = 12;
const WAIT_MS = 2000;
const POLL_MS = 25;
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
        const now = await shown();
        if (now.focus === focus || now.item === focus) return;
      }
      assert.fail(`Tab never reached ${focus}`);
    };
    const until = async (done, what) => {
      const deadline = Date.now() + WAIT_MS;
      while (!done(await evaluate("Fixture.shown()"))) {
        assert.ok(Date.now() < deadline, `${what} within ${WAIT_MS} ms`);
        await pause(POLL_MS);
      }
    };
    const update = (next) => evaluate(`Fixture.update(${JSON.stringify(next)})`);
    return steps({ shown, hover, away, click, key, tabTo, update, until });
  });
}

test("hovering the bottom-left block opens the user menu with its five items, and moving away closes it", async () => {
  await onSidebar({}, async ({ shown, hover, away }) => {
    assert.equal((await shown()).menu, false, "no menu before the pointer comes");
    await hover(FOOT);
    const open = await shown();
    assert.equal(open.menu, true);
    assert.deepEqual(open.items, ALL);
    assert.equal(open.expanded, "true");
    assert.equal(open.disabled, false);
    await hover(ITEM);
    assert.equal((await shown()).menu, true, "the menu stays while the pointer is on it");
    await away();
    assert.equal((await shown()).menu, false);
  });
});

test("keyboard focus opens the same menu, Tab reaches its first item, and Esc closes it with focus kept in the block", async () => {
  await onSidebar({}, async ({ shown, key, tabTo }) => {
    assert.equal((await shown()).menu, false, "no menu before focus comes");
    await tabTo("me");
    assert.equal((await shown()).menu, true, "focus on the profile name opens the menu");
    await key("Tab", 9);
    const first = await shown();
    assert.equal(first.focus, "menuitem");
    assert.equal(first.item, "profile");
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
    await tabTo("restart");
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
    assert.deepEqual(held.items, ALL);
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

test("at rest the block draws no menu and no dialog, and its name is a button that opens the menu, not a link", () => {
  const restart = { held: false, busy: false, failure: "", onRestart: forbidden };
  const menu = { open: null, onOpen: forbidden };
  const markup = renderToStaticMarkup(createElement(shell.Sidebar, { route: { name: "home" }, projects: [], profileName: "Mira Okafor", engine: "running", onRetry: forbidden, restart, menu }));
  assert.doesNotMatch(markup, /role="menu"|<dialog|Restart engine|Projects folder/);
  assert.match(markup, /<button type="button" class="me" aria-haspopup="menu" aria-expanded="false">Mira Okafor<\/button>/);
  assert.doesNotMatch(markup, /#\/settings\/profile/);
  assert.match(markup, /Engine running/);
});

test("a footer without a profile name is a programming error", async () => {
  const foot = await import("../src/ui/sidebar-foot.tsx");
  assert.throws(() => renderToStaticMarkup(createElement(foot.SidebarFoot, { name: "", engine: "running", onRetry: forbidden })), /the footer names a profile/);
});

test("without a restart control the block keeps its words and has no menu", () => {
  const markup = renderToStaticMarkup(createElement(shell.Sidebar, { route: { name: "home" }, projects: [], profileName: "", engine: "down", onRetry: forbidden }));
  assert.match(markup, /<span class="me">Profile<\/span>/);
  assert.match(markup, /Try again/);
  assert.match(markup, /Engine not running/);
});

test("a click on the name opens the menu and never leaves the page", async () => {
  await onSidebar({}, async ({ shown, hover, click, key, until }) => {
    await hover(FOOT);
    await until((now) => now.menu, "the hovered menu");
    await key("Escape", 27);
    assert.equal((await shown()).menu, false, "Esc closed the hovered menu");
    await click(NAME);
    const open = await shown();
    assert.equal(open.menu, true);
    assert.deepEqual(open.items, ALL);
    assert.equal(open.hash, "#/home", "the click navigates nowhere");
    assert.equal(open.page, "Body");
    assert.deepEqual(open.events, []);
  });
});

for (const [tab, label] of USER_ITEMS) {
  test(`${label} opens its dialog over the page, and Esc closes it back to the same page with focus on the name`, async () => {
    await onSidebar({}, async ({ shown, hover, click, key }) => {
      await hover(FOOT);
      await click(`.sidebar-foot [data-item=${tab}]`);
      const open = await shown();
      assert.equal(open.dialog?.title, label);
      assert.equal(open.dialogs, 1);
      assert.equal(open.menu, false, "the menu closes under its dialog");
      assert.equal(open.hash, "#/home");
      assert.equal(open.page, "Body");
      await key("Escape", 27);
      const back = await shown();
      assert.equal(back.dialog, null);
      assert.equal(back.menu, false);
      assert.equal(back.focus, "me");
      assert.equal(back.hash, "#/home");
      assert.equal(back.page, "Body");
      assert.deepEqual(back.events, [`open:${tab}`, `close:${tab}`]);
    });
  });
}

test("the dialog's Close button also closes back to the same page, and the menu opens again on the next visit", async () => {
  await onSidebar({}, async ({ shown, hover, away, click }) => {
    await hover(FOOT);
    await click(".sidebar-foot [data-item=github]");
    await click("dialog[open] .dialog-head button");
    const back = await shown();
    assert.equal(back.dialog, null);
    assert.equal(back.menu, false);
    assert.equal(back.hash, "#/home");
    assert.deepEqual(back.events, ["open:github", "close:github"]);
    await away();
    await hover(FOOT);
    assert.equal((await shown()).menu, true);
  });
});

test("arrow keys move through the five items and wrap, and Enter opens the focused one", async () => {
  await onSidebar({}, async ({ shown, key, tabTo }) => {
    await tabTo("me");
    const step = async (name, code) => { await key(name, code); return (await shown()).item; };
    assert.equal(await step("ArrowDown", 40), "profile", "down from the name reaches the first item");
    assert.equal(await step("ArrowDown", 40), "updates");
    assert.equal(await step("ArrowUp", 38), "profile");
    assert.equal(await step("ArrowUp", 38), "restart", "up from the first item wraps to the last");
    assert.equal(await step("ArrowDown", 40), "profile", "down from the last wraps to the first");
    await step("ArrowDown", 40);
    assert.equal(await step("ArrowDown", 40), "github");
    await key("Enter", 13, "\r");
    const open = await shown();
    assert.equal(open.dialog?.title, "GitHub");
    assert.deepEqual(open.events, ["open:github"]);
    await key("Escape", 27);
    const back = await shown();
    assert.equal(back.dialog, null);
    assert.equal(back.menu, false);
    assert.equal(back.focus, "me");
  });
});

test("up from the name skips a Restart engine held off by an update install", async () => {
  await onSidebar({ held: true }, async ({ shown, key, tabTo }) => {
    await tabTo("me");
    await key("ArrowUp", 38);
    assert.equal((await shown()).item, "home");
  });
});

test("without the user menu control the menu holds Restart engine alone", async () => {
  await onSidebar({ menu: false }, async ({ shown, hover }) => {
    await hover(FOOT);
    assert.deepEqual((await shown()).items, ["Restart engine"]);
  });
});
