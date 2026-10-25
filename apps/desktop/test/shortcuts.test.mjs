import assert from "node:assert/strict";
import test from "node:test";
import { bundle, withPage } from "./browser.mjs";

const fixture = await bundle(new URL("./fixtures/shortcuts.mjs", import.meta.url).pathname);
const CTRL = 2;
const META = 4;

async function press(send, key, modifiers) {
  for (const type of ["rawKeyDown", "keyUp"]) {
    await send("Input.dispatchKeyEvent", { type, key, code: key === "/" ? "Slash" : `Key${key.toUpperCase()}`, modifiers, windowsVirtualKeyCode: key === "/" ? 191 : key.toUpperCase().charCodeAt(0) });
  }
}

async function onPlatform(platform, check) {
  return withPage(async ({ evaluate, send }) => {
    const userAgent = await evaluate("navigator.userAgent");
    await send("Emulation.setUserAgentOverride", { userAgent, platform });
    await evaluate(fixture);
    assert.equal(await evaluate("navigator.platform"), platform);
    await evaluate("Fixture.mountGoTo()");
    const step = async (focus, key, modifiers) => {
      await evaluate(`document.querySelector(${JSON.stringify(focus)}).focus()`);
      await press(send, key, modifiers);
      await evaluate("new Promise((done) => setTimeout(done, 0))");
      const open = await evaluate("Fixture.openDialog()");
      const keys = await evaluate("Fixture.shortcutKeys()");
      await evaluate("Fixture.mountGoTo()");
      return { open, keys: keys.slice(0, 2) };
    };
    return check(step);
  });
}

test("on Windows and Linux, Ctrl opens Go to and the shortcuts list, and the list says Ctrl", async () => {
  for (const platform of ["Win32", "Linux x86_64"]) {
    const seen = await onPlatform(platform, async (step) => ({
      ctrlK: await step("#field", "k", CTRL),
      ctrlSlash: await step("#field", "/", CTRL),
      metaK: await step("#field", "k", META),
      terminal: await step(".xterm-helper-textarea", "k", CTRL),
    }));
    assert.deepEqual(seen, {
      ctrlK: { open: "Go to", keys: [] },
      ctrlSlash: { open: "Keyboard shortcuts", keys: ["Ctrl+K", "Ctrl+/"] },
      metaK: { open: null, keys: [] },
      terminal: { open: null, keys: [] },
    }, platform);
  }
});

test("on macOS, Cmd opens Go to and the shortcuts list, Ctrl doesn't, and the list says Cmd", async () => {
  const seen = await onPlatform("MacIntel", async (step) => ({
    metaK: await step("#field", "k", META),
    metaSlash: await step("#field", "/", META),
    ctrlK: await step("#field", "k", CTRL),
  }));
  assert.deepEqual(seen, {
    metaK: { open: "Go to", keys: [] },
    metaSlash: { open: "Keyboard shortcuts", keys: ["Cmd+K", "Cmd+/"] },
    ctrlK: { open: null, keys: [] },
  });
});
