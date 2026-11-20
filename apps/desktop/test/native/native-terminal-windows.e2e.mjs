import assert from "node:assert/strict";

const RECT = { x: 40, y: 120, width: 640, height: 400 };
const THEME = {
  background: "#101010", foreground: "#e0e0e0", cursor: "#e0e0e0", cursorAccent: "#101010",
  selectionBackground: "#335588", black: "#000000", red: "#cc0000", green: "#00cc00",
  yellow: "#cccc00", blue: "#0000cc", magenta: "#cc00cc", cyan: "#00cccc", white: "#cccccc",
  brightBlack: "#555555", brightRed: "#ff5555", brightGreen: "#55ff55", brightYellow: "#ffff55",
  brightBlue: "#5555ff", brightMagenta: "#ff55ff", brightCyan: "#55ffff", brightWhite: "#ffffff",
};
const STYLE = { fontFamily: "Consolas", fontSize: 13, cursorBlink: false, theme: THEME };
const IPC_READY_MS = 30000;

/** Calls one app command from the page through Tauri's own IPC, as the frontend does.
 * Inputs: the command and its arguments. Output: { value } or { failed }, the refusal as
 * the page received it, serialized. */
function call(command, args) {
  return browser.executeAsync((command, args, done) => {
    window.__TAURI_INTERNALS__.invoke(command, args).then(
      (value) => done({ value }),
      (failure) => done({ failed: JSON.stringify(failure) }),
    );
  }, command, args);
}

describe("the Windows native terminal in the real app, before the flag", () => {
  before(async () => {
    await browser.waitUntil(
      () => browser.execute(() => typeof window.__TAURI_INTERNALS__?.invoke === "function"),
      { timeout: IPC_READY_MS, timeoutMsg: "the real frontend has Tauri IPC" },
    );
  });

  it("loads the real frontend and reports its display", async () => {
    assert.equal(await browser.getTitle(), "CrazyAgent");
    const shown = await browser.execute(() => ({
      screen: [window.screen.width, window.screen.height],
      inner: [window.innerWidth, window.innerHeight],
      ratio: window.devicePixelRatio,
    }));
    console.log(`h5c row: page screen ${shown.screen.join("x")} inner ${shown.inner.join("x")} ratio ${shown.ratio}`);
    assert.ok(shown.inner[0] > 0 && shown.inner[1] > 0, "the window has a page area");
  });

  it("answers that this build draws no native terminal yet", async () => {
    const answer = await call("desktop_native_terminal_enabled", {});
    assert.equal(answer.failed, undefined, `enabled answered: ${answer.failed}`);
    assert.equal(answer.value, false, "Windows native views stay off until the flag");
  });

  it("refuses a valid native open at the guard, before any engine call", async () => {
    const tag = await browser.execute(() => window.__TAURI_INTERNALS__.transformCallback(() => {}));
    const answer = await call("desktop_native_terminal_open", {
      name: "h5c-guard", pane: null, label: "H5c guard row", rect: RECT, style: STYLE,
      covered: false, keys: false, project: null, events: `__CHANNEL__:${tag}`,
    });
    assert.equal(answer.value, undefined, `the open answered a view: ${answer.value}`);
    console.log(`h5c row: open refused ${answer.failed}`);
    assert.match(answer.failed, /"native_terminal_unsupported"/, "the guard's code, not an engine one");
  });
});
