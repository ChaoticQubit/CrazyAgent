import assert from "node:assert/strict";
import test from "node:test";
import { bundle, withPage } from "./browser.mjs";

const fixture = await bundle(new URL("./fixtures/native-panel.mjs", import.meta.url).pathname);

const THEME = ["background", "black", "blue", "brightBlack", "brightBlue", "brightCyan", "brightGreen", "brightMagenta", "brightRed", "brightWhite", "brightYellow",
  "cursor", "cursorAccent", "cyan", "foreground", "green", "magenta", "red", "selectionBackground", "white", "yellow"];

/** Runs one script in a fresh 1200x800 page that holds the native panel fixture as `F`.
 * Input: the body of an async function. Output: what it returns. */
async function page(script) {
  return withPage(async ({ evaluate, send }) => {
    await send("Emulation.setDeviceMetricsOverride", { width: 1200, height: 800, deviceScaleFactor: 1, mobile: false });
    await evaluate(fixture);
    return evaluate(`(async () => { const F = Fixture; const pause = (ms) => new Promise((done) => setTimeout(done, ms)); ${script} })()`);
  });
}

const MASTER = `F.agent("master", "Master", "team-1")`;

test("V243-NATIVE-ONLY: with the native kind a panel opens one native view over its screen in the app's style, says where typing goes once ready, and never reaches the webview terminal", async () => {
  const seen = await page(`
    await F.stage();
    F.mount("a", ${MASTER});
    await F.until(() => F.calls("desktop_native_terminal_open").length === 1);
    const open = F.calls("desktop_native_terminal_open")[0].args;
    const opening = F.screenRect("a");
    F.event(1, { type: "ready", cols: 80, rows: 24 });
    await F.until(() => F.shown("a").status.some((line) => line.startsWith("Typing goes straight to the master.")));
    await pause(700);
    return { open, opening, rect: F.screenRect("a"), place: F.calls("desktop_native_terminal_place").at(-1)?.args ?? null, shown: F.shown("a") };
  `);
  assert.equal(seen.open.name, "team-1");
  assert.equal(seen.open.pane, null);
  assert.equal(seen.open.label, "Master");
  assert.deepEqual(seen.open.rect, seen.opening);
  assert.ok(seen.opening.width > 0 && seen.opening.height > 0, JSON.stringify(seen.opening));
  assert.equal(seen.open.style.fontSize, 13);
  assert.equal(seen.open.style.cursorBlink, true);
  assert.ok(seen.open.style.fontFamily.length > 0);
  assert.deepEqual(Object.keys(seen.open.style.theme).sort(), THEME);
  assert.ok(Object.values(seen.open.style.theme).every((color) => typeof color === "string" && color.length > 0));
  assert.deepEqual(seen.place ?? { view: 1, rect: seen.opening }, { view: 1, rect: seen.rect }, "the view sits where the screen is now");
  assert.equal(seen.shown.xterms, 0, "no xterm and no webview terminal host");
  assert.equal(seen.shown.webview, 0, "no webview attach, input, resize or detach");
});

test("V243-NATIVE-ONLY: with the native kind the app-start preload opens the remembered terminal covered, and showing it places and uncovers it without a second open or any webview call", async () => {
  const seen = await page(`
    await F.stage({ nodes: [${MASTER}] });
    F.preload([{ key: { project: "p1", terminal: "team-1", pane: null }, size: { width: 640, height: 380 } }], ["p1"]);
    if (!(await F.until(() => F.calls("desktop_native_terminal_open").length === 1))) return { loaded: false, saved: F.saved() };
    const open = F.calls("desktop_native_terminal_open")[0].args;
    F.mount("a", ${MASTER});
    const uncovered = await F.until(() => F.calls("desktop_native_terminal_cover").some((entry) => entry.args.view === 1 && entry.args.covered === false));
    await pause(300);
    return { loaded: true, open, uncovered, opens: F.calls("desktop_native_terminal_open").length, placed: F.calls("desktop_native_terminal_place").some((entry) => entry.args.view === 1), shown: F.shown("a") };
  `);
  assert.equal(seen.loaded, true, JSON.stringify(seen));
  assert.equal(seen.open.covered, true, "a preloaded view draws nothing until a panel shows it");
  assert.equal(seen.uncovered, true);
  assert.equal(seen.placed, true);
  assert.equal(seen.opens, 1);
  assert.equal(seen.shown.xterms, 0);
  assert.equal(seen.shown.webview, 0);
});

test("V243-NATIVE-ONLY: a desktop whose terminal answer cannot be read shows the plain reason in the panel, opens nothing and never falls back to the webview terminal", async () => {
  const seen = await page(`
    await F.stage({ enabled: "Command desktop_native_terminal_enabled not found" });
    F.mount("a", ${MASTER});
    await pause(400);
    return { shown: F.shown("a"), opens: F.calls("desktop_native_terminal_open").length };
  `);
  assert.deepEqual(seen.shown.alerts, ["The terminal is not ready yet."]);
  assert.equal(seen.opens, 0);
  assert.equal(seen.shown.xterms, 0);
  assert.equal(seen.shown.webview, 0);
});

test("V243-NATIVE-ONLY: a right click on the native view opens the terminal menu with the view covered so the menu shows, Copy and Copy output copy through the view, and the view shows again after", async () => {
  const seen = await page(`
    await F.stage();
    const view = await F.live("a", ${MASTER});
    const rect = F.screenRect("a");
    F.event(view, { type: "menu", x: rect.x + 30, y: rect.y + 30 });
    const menu = await F.until(() => F.shown("a").menu.length === 2) ? F.shown("a").menu : null;
    const covered = await F.until(() => F.calls("desktop_native_terminal_cover").at(-1)?.args.covered === true);
    F.choose("a", "Copy");
    const selection = await F.until(() => F.shown("a").status.includes("Copied the selection."));
    const back = await F.until(() => F.shown("a").menu.length === 0 && F.calls("desktop_native_terminal_cover").at(-1)?.args.covered === false);
    F.event(view, { type: "menu", x: rect.x + 30, y: rect.y + 30 });
    await F.until(() => F.shown("a").menu.length === 2);
    F.choose("a", "Copy output");
    const output = await F.until(() => F.shown("a").status.includes("Copied the output."));
    return { view, menu, covered, selection, back, output, copies: F.calls("desktop_native_terminal_copy").map((entry) => entry.args) };
  `);
  assert.equal(seen.view, 1);
  assert.deepEqual(seen.menu, ["Copy", "Copy output"]);
  assert.equal(seen.covered, true, "the view is covered while the page menu is open");
  assert.equal(seen.selection, true);
  assert.equal(seen.back, true, "the menu closes and the view shows again");
  assert.equal(seen.output, true);
  assert.deepEqual(seen.copies, [{ view: 1, what: "selection" }, { view: 1, what: "output" }]);
});

test("V243-NATIVE-ONLY: a copy the view refuses says why in the status line", async () => {
  const seen = await page(`
    await F.stage();
    await F.live("a", ${MASTER});
    F.fail("copy", { code: "native_terminal_nothing_to_copy" });
    F.press("a", "Copy output");
    const words = F.words("native_terminal_nothing_to_copy", "terminal_ended");
    return { said: await F.until(() => F.shown("a").status.includes(words.refused)), words, shown: F.shown("a") };
  `);
  assert.equal(seen.said, true, JSON.stringify(seen));
});

test("V243-NATIVE-ONLY: while the app in the native view reads the mouse, the status line says how to select text, and stops once it does not", async () => {
  const seen = await page(`
    await F.stage();
    const view = await F.live("a", ${MASTER});
    const words = F.words("x", "terminal_ended");
    F.event(view, { type: "mouse", on: true });
    const on = await F.until(() => F.shown("a").status.some((line) => line.endsWith(words.select)));
    F.event(view, { type: "mouse", on: false });
    const off = await F.until(() => !F.shown("a").status.some((line) => line.endsWith(words.select)));
    return { on, off };
  `);
  assert.deepEqual(seen, { on: true, off: true });
});

test("V243-NATIVE-ONLY: Jump to latest shows only while the native view is scrolled up and scrolls it back through the view", async () => {
  const seen = await page(`
    await F.stage();
    const view = await F.live("a", ${MASTER});
    const before = F.shown("a").buttons.includes("Jump to latest");
    F.event(view, { type: "behind", behind: true });
    const shows = await F.until(() => F.shown("a").buttons.includes("Jump to latest"));
    F.press("a", "Jump to latest");
    const jumped = await F.until(() => F.calls("desktop_native_terminal_jump").length === 1);
    F.event(view, { type: "behind", behind: false });
    const gone = await F.until(() => !F.shown("a").buttons.includes("Jump to latest"));
    return { before, shows, jumped, gone, jump: F.calls("desktop_native_terminal_jump")[0]?.args ?? null };
  `);
  assert.deepEqual(seen, { before: false, shows: true, jumped: true, gone: true, jump: { view: 1 } });
});

test("V243-NATIVE-ONLY: a following panel whose agent ended opens a new view, says which try it is and why, and never calls the ended view again", async () => {
  const seen = await page(`
    await F.stage();
    const view = await F.live("a", ${MASTER}, { follow: true });
    const mark = F.calls().length;
    F.event(view, { type: "ended", code: "terminal_ended", final: false });
    const words = F.words("x", "terminal_ended");
    const line = "Connecting to the terminal again, try 1 of 30. Last try: " + words.ended;
    const trying = await F.until(() => F.shown("a").status.includes(line));
    const reopened = await F.until(() => F.calls("desktop_native_terminal_open").length === 2, 3000);
    await pause(300);
    F.event(view, { type: "behind", behind: true });
    await pause(200);
    return { trying, reopened, onEnded: F.callsOn(view, mark), status: F.shown("a").status, stale: F.shown("a").buttons.includes("Jump to latest") };
  `);
  assert.equal(seen.trying, true, JSON.stringify(seen.status));
  assert.equal(seen.reopened, true);
  assert.deepEqual(seen.onEnded, [], "an ended view is gone: no place, cover or close on it");
  assert.equal(seen.stale, false, "an event of the ended view changes nothing");
});

test("V243-NATIVE-ONLY: an open the desktop refuses after its view already ended counts as one end, so a following panel tries once, not twice", async () => {
  const seen = await page(`
    await F.stage();
    F.hold();
    F.mount("a", ${MASTER}, { follow: true });
    await F.until(() => F.calls("desktop_native_terminal_open").length === 1);
    F.event(1, { type: "ended", code: "terminal_ended", final: false });
    const words = F.words("x", "terminal_ended");
    const first = await F.until(() => F.shown("a").status.includes("Connecting to the terminal again, try 1 of 30. Last try: " + words.ended));
    F.release({ code: "terminal_ended" });
    await pause(300);
    const status = F.shown("a").status;
    const reopened = await F.until(() => F.calls("desktop_native_terminal_open").length === 2, 3000);
    await pause(1500);
    return { first, status, reopened, opens: F.calls("desktop_native_terminal_open").length };
  `);
  assert.equal(seen.first, true);
  assert.ok(seen.status.some((line) => line.includes("try 1 of 30")), JSON.stringify(seen.status));
  assert.ok(!seen.status.some((line) => line.includes("try 2 of 30")), JSON.stringify(seen.status));
  assert.equal(seen.reopened, true);
  assert.equal(seen.opens, 2, "one end, one new open");
});

test("V243-NATIVE-ONLY: after a prompt stops reaching the agent and the owner opens the terminal again, the message box sends again", async () => {
  const seen = await page(`
    await F.stage();
    F.input("box");
    const view = await F.live("a", ${MASTER});
    F.fail("prompt", { code: "terminal_gone" });
    const first = await F.promptBox("a", "first");
    const stopped = await F.until(() => F.shown("a").alerts.some((line) => line.startsWith("Typing stopped reaching this agent.")));
    F.fail("prompt", null);
    F.press("a", "Open again");
    await F.until(() => F.calls("desktop_native_terminal_open").length === 2);
    F.event(2, { type: "ready", cols: 80, rows: 24 });
    await F.until(() => F.shown("a").status.some((line) => line.startsWith("The terminal shows Master")));
    const second = await F.promptBox("a", "second");
    const sent = await F.until(() => F.calls("desktop_terminal_prompt").some((entry) => entry.args.text === "second"));
    return { view, first, stopped, second, sent, prompts: F.calls("desktop_terminal_prompt").map((entry) => entry.args.text) };
  `);
  assert.deepEqual(seen, { view: 1, first: true, stopped: true, second: true, sent: true, prompts: ["first", "second"] });
});

test("V243-NATIVE-ONLY: a native view that ended for good shows why with Open again, does not follow, and Open again opens a new view", async () => {
  const seen = await page(`
    await F.stage();
    const view = await F.live("a", ${MASTER}, { follow: true });
    F.event(view, { type: "ended", code: "terminal_attach_failed", final: true });
    const words = F.words("x", "terminal_attach_failed");
    const ended = await F.until(() => F.shown("a").alerts.some((line) => line.startsWith(words.ended)));
    await pause(1500);
    const opensBefore = F.calls("desktop_native_terminal_open").length;
    const pressed = F.press("a", "Open again");
    const reopened = await F.until(() => F.calls("desktop_native_terminal_open").length === 2);
    return { ended, opensBefore, pressed, reopened };
  `);
  assert.deepEqual(seen, { ended: true, opensBefore: 1, pressed: true, reopened: true });
});

test("V243-NATIVE-ONLY: a closed panel keeps its native view covered, the third agent shown closes the least recently used hidden view, and showing a kept one again opens nothing (DEC-187)", async () => {
  const seen = await page(`
    await F.stage();
    const one = await F.live("a", F.agent("sub", "Ada", "team-1"));
    F.unmount("a");
    await F.until(() => F.calls("desktop_native_terminal_cover").at(-1)?.args.covered === true);
    const parked = F.calls("desktop_native_terminal_cover").at(-1)?.args ?? null;
    const two = await F.live("a", F.agent("sub", "Brook", "team-2"));
    F.unmount("a");
    const mark = F.calls().length;
    const three = await F.live("a", F.agent("sub", "Cove", "team-3"));
    F.unmount("a");
    const closed = F.calls("desktop_native_terminal_close").map((entry) => entry.args.view);
    F.mount("a", F.agent("sub", "Brook", "team-2"));
    const shownAgain = await F.until(() => F.calls("desktop_native_terminal_cover").at(-1)?.args.view === two && F.calls("desktop_native_terminal_cover").at(-1)?.args.covered === false);
    return { views: [one, two, three], parked, closed, shownAgain, opens: F.calls("desktop_native_terminal_open").length, sinceMark: F.callsOn(one, mark) };
  `);
  assert.deepEqual(seen.views, [1, 2, 3]);
  assert.deepEqual(seen.parked, { view: 1, covered: true });
  assert.deepEqual(seen.closed, [1]);
  assert.deepEqual(seen.sinceMark, ["desktop_native_terminal_close"]);
  assert.equal(seen.shownAgain, true);
  assert.equal(seen.opens, 3);
});

test("V243-NATIVE-ONLY: a page dialog over the native view covers it until the dialog goes", async () => {
  const seen = await page(`
    await F.stage();
    const view = await F.live("a", ${MASTER});
    F.dialog(true);
    const covered = await F.until(() => F.calls("desktop_native_terminal_cover").at(-1)?.args.covered === true);
    F.dialog(false);
    const uncovered = await F.until(() => F.calls("desktop_native_terminal_cover").at(-1)?.args.covered === false);
    return { view, covered, uncovered, last: F.calls("desktop_native_terminal_cover").at(-1)?.args ?? null };
  `);
  assert.deepEqual(seen, { view: 1, covered: true, uncovered: true, last: { view: 1, covered: false } });
});

test("V243-NATIVE-ONLY: a native view let go before its open answers is closed once the answer comes, and is never placed or covered", async () => {
  const seen = await page(`
    await F.stage();
    F.hold();
    F.mount("a", ${MASTER});
    await F.until(() => F.calls("desktop_native_terminal_open").length === 1);
    F.unmount("a");
    F.letGoAll();
    const released = F.release();
    const closed = await F.until(() => F.calls("desktop_native_terminal_close").length === 1);
    await pause(300);
    return { released, closed, onView: F.callsOn(1, 0) };
  `);
  assert.deepEqual(seen, { released: 1, closed: true, onView: ["desktop_native_terminal_close"] });
});

test("V243-NATIVE-ONLY: a wall tile opens its own native view and closes it with the tile", async () => {
  const seen = await page(`
    await F.stage();
    const view = await F.live("a", ${MASTER}, { tile: true });
    F.unmount("a");
    const closed = await F.until(() => F.calls("desktop_native_terminal_close").some((entry) => entry.args.view === view));
    return { view, closed };
  `);
  assert.deepEqual(seen, { view: 1, closed: true });
});

test("V243-NATIVE-ONLY: an agent with no terminal yet opens no native view, and a desktop without the native view command shows Not ready, never the webview terminal", async () => {
  const seen = await page(`
    await F.stage();
    F.mount("a", F.agent("sub", "Ada", null));
    await pause(300);
    const none = { shown: F.shown("a"), opens: F.calls("desktop_native_terminal_open").length };
    F.unmount("a");
    F.fail("open", "Command desktop_native_terminal_open not found");
    F.mount("b", ${MASTER});
    const notReady = await F.until(() => document.querySelector("#b .not-ready") !== null);
    return { none, notReady, shown: F.shown("b") };
  `);
  assert.ok(seen.none.status.includes("This agent has no terminal yet. It opens once the agent starts."), JSON.stringify(seen.none));
  assert.equal(seen.none.opens, 0);
  assert.equal(seen.notReady, true);
  assert.equal(seen.shown.webview, 0);
  assert.equal(seen.shown.xterms, 0);
});
