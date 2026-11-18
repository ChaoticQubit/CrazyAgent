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
  assert.ok(seen.none.shown.status.includes("This agent has no terminal yet. It opens once the agent starts."), JSON.stringify(seen.none));
  assert.equal(seen.none.opens, 0);
  assert.equal(seen.notReady, true);
  assert.equal(seen.shown.webview, 0);
  assert.equal(seen.shown.xterms, 0);
});

test("V243-NATIVE-ONLY: a native view in a panel of fractional size is remembered at a whole-pixel size, so the next app start still reads it (#498 F2)", async () => {
  const seen = await page(`
    await F.stage();
    F.slot("a", "640.5px", "420.25px");
    const view = await F.live("a", ${MASTER});
    const rect = F.screenRect("a");
    const kept = await F.until(() => F.saved().length === 1 && F.saved()[0].size !== null);
    return { view, rect, kept, saved: F.saved(), stored: window.localStorage.getItem("crazyagent.kept-terminals") };
  `);
  assert.equal(seen.view, 1);
  assert.ok(!Number.isInteger(seen.rect.width) || !Number.isInteger(seen.rect.height), JSON.stringify(seen.rect));
  assert.equal(seen.kept, true, seen.stored);
  assert.deepEqual(seen.saved[0].key, { project: "p1", terminal: "team-1", pane: null });
  assert.ok(Number.isInteger(seen.saved[0].size.width) && Number.isInteger(seen.saved[0].size.height), JSON.stringify(seen.saved));
  assert.ok(Math.abs(seen.saved[0].size.width - seen.rect.width) <= 1 && Math.abs(seen.saved[0].size.height - seen.rect.height) <= 1, JSON.stringify(seen));
});

test("V243-NATIVE-ONLY: the app's toast over the native view covers it until the toast goes (#498 F3)", async () => {
  const seen = await page(`
    await F.stage();
    const view = await F.live("a", ${MASTER});
    F.toast(true);
    const covered = await F.until(() => F.calls("desktop_native_terminal_cover").at(-1)?.args.covered === true);
    F.toast(false);
    const uncovered = await F.until(() => F.calls("desktop_native_terminal_cover").at(-1)?.args.covered === false);
    return { view, covered, uncovered };
  `);
  assert.deepEqual(seen, { view: 1, covered: true, uncovered: true });
});

test("V243-NATIVE-ONLY: a console seed is pasted into the native view once it is live, through the view's paste, and only then marked used (DEC-203 seed exception)", async () => {
  const seen = await page(`
    await F.stage();
    window.seedUsed = 0;
    const seed = { id: 7, text: "check the build\\nthen report", used: () => { window.seedUsed += 1; } };
    F.mount("a", ${MASTER}, { seed });
    await F.until(() => F.calls("desktop_native_terminal_open").length === 1);
    await pause(300);
    const beforeReady = { pastes: F.calls("desktop_native_terminal_paste").length, used: window.seedUsed };
    F.event(1, { type: "ready", cols: 80, rows: 24 });
    const pasted = await F.until(() => F.calls("desktop_native_terminal_paste").length === 1 && window.seedUsed === 1);
    F.rerender("a", ${MASTER}, { seed });
    await pause(300);
    return { beforeReady, pasted, pastes: F.calls("desktop_native_terminal_paste").map((entry) => entry.args), used: window.seedUsed, webview: F.shown("a").webview };
  `);
  assert.deepEqual(seen.beforeReady, { pastes: 0, used: 0 }, "nothing is pasted before the view is live");
  assert.equal(seen.pasted, true);
  assert.deepEqual(seen.pastes, [{ view: 1, text: "check the build\nthen report" }], "the page sends the draft as is; the desktop flattens it for an unbracketed pane");
  assert.equal(seen.used, 1, "used fires once, after the paste was taken");
  assert.equal(seen.webview, 0);
});

test("V243-NATIVE-ONLY: a native view whose ready event comes before the desktop answers its open goes live only with the answer, so copy, jump and paste always name a view", async () => {
  const seen = await page(`
    await F.stage();
    F.hold();
    F.mount("a", ${MASTER});
    await F.until(() => F.calls("desktop_native_terminal_open").length === 1);
    F.event(1, { type: "ready", cols: 80, rows: 24 });
    await pause(300);
    const early = document.querySelector("#a .tg-terminal-note") !== null;
    F.release();
    const live = await F.until(() => document.querySelector("#a .tg-terminal-note") !== null);
    F.press("a", "Copy output");
    const copied = await F.until(() => F.calls("desktop_native_terminal_copy").length === 1);
    return { early, live, copied, copy: F.calls("desktop_native_terminal_copy")[0]?.args ?? null };
  `);
  assert.deepEqual(seen, { early: false, live: true, copied: true, copy: { view: 1, what: "output" } });
});

for (const saved of [null, "terminal", "box", "both"]) {
  test(`V243-NATIVE-INPUT E1: with Terminal input ${saved} saved, the native view takes typed keys only when the choice lets them through, each open carries the choice, and a choice changed before the next render reaches the open view`, async () => {
    const keysOn = saved !== "box";
    const flip = keysOn ? "box" : "both";
    const seen = await page(`
      await F.stage();
      ${saved === null ? "" : `F.input(${JSON.stringify(saved)});`}
      const view = await F.live("a", ${MASTER});
      await pause(200);
      const keys = () => F.calls("desktop_native_terminal_keys").map((entry) => entry.args);
      const first = keys();
      const boxes = document.querySelectorAll("#a textarea.tg-composer-input").length;
      F.event(view, { type: "ended", code: "terminal_attach_failed", final: true });
      await F.until(() => F.shown("a").buttons.includes("Open again"));
      F.press("a", "Open again");
      await F.until(() => F.calls("desktop_native_terminal_open").length === 2);
      F.event(2, { type: "ready", cols: 80, rows: 24 });
      await F.until(() => document.querySelector("#a .tg-terminal-note") !== null);
      await pause(200);
      const reopened = keys();
      F.input(${JSON.stringify(flip)});
      F.rerender("a", ${MASTER});
      await pause(200);
      return { view, first, boxes, reopened, changed: keys(), opened: F.calls("desktop_native_terminal_open").map((entry) => entry.args.keys) };
    `);
    assert.equal(seen.view, 1);
    assert.deepEqual(seen.opened, [keysOn, keysOn], "each open, the first and Open again's, carries the choice (rv-sec2 #509 N1)");
    assert.deepEqual(seen.first, [], "the open carried the choice, so no keys call follows it");
    assert.equal(seen.boxes, saved === "box" || saved === "both" ? 1 : 0, "the message box shows as in the web panel");
    assert.deepEqual(seen.reopened, [], "the view Open again makes carried the choice in its open too");
    assert.deepEqual(seen.changed, [{ view: 2, on: !keysOn }], "the choice read at the next render reaches the open view once");
  });
}

test("V243-NATIVE-INPUT E2: with Both saved, the master's message box hands its text to the engine once and empties, then gives the native view the keyboard", async () => {
  const seen = await page(`
    await F.stage();
    F.input("both");
    const view = await F.live("a", ${MASTER});
    await pause(200);
    const from = F.calls().length;
    const sent = await F.promptBox("a", "Build the landing page");
    const emptied = await F.until(() => document.querySelector("#a textarea.tg-composer-input").value === "" && F.calls("desktop_terminal_prompt").length === 1);
    await F.until(() => F.callsOn(view, from).includes("desktop_native_terminal_focus"));
    await pause(100);
    return { view, sent, emptied, prompts: F.calls("desktop_terminal_prompt").map((entry) => entry.args.text), focus: F.callsOn(view, from).filter((command) => command === "desktop_native_terminal_focus").length };
  `);
  assert.deepEqual(seen, { view: 1, sent: true, emptied: true, prompts: ["Build the landing page"], focus: 1 });
});

test("V243-NATIVE-INPUT E4: a native view gets the keyboard once when it goes live while shown and once each time it shows again live, and none while a page dialog covers it until the dialog goes", async () => {
  const seen = await page(`
    await F.stage();
    const focus = () => F.calls("desktop_native_terminal_focus").map((entry) => entry.args);
    const view = await F.live("a", ${MASTER});
    await pause(200);
    const live = focus();
    F.unmount("a");
    await pause(100);
    const hidden = focus().length;
    F.mount("a", ${MASTER});
    await pause(200);
    const again = focus();
    F.unmount("a");
    F.dialog(true);
    F.mount("a", ${MASTER});
    await pause(300);
    const covered = focus().length;
    F.dialog(false);
    await F.until(() => F.calls("desktop_native_terminal_cover").at(-1)?.args.covered === false);
    await pause(100);
    return { view, live, hidden, again, covered, after: focus() };
  `);
  assert.equal(seen.view, 1);
  assert.deepEqual(seen.live, [{ view: 1 }], "going live while shown gives the view the keyboard once");
  assert.equal(seen.hidden, 1, "hiding the view gives it nothing");
  assert.deepEqual(seen.again, [{ view: 1 }, { view: 1 }], "showing the kept live view again gives it the keyboard once more");
  assert.equal(seen.covered, 2, "nothing while a page dialog covers the view");
  assert.deepEqual(seen.after, [{ view: 1 }, { view: 1 }, { view: 1 }], "the keyboard goes to the view once the dialog goes");
});

test("V243-NATIVE-INPUT E4: Jump to latest and leaving the terminal menu with Escape give the native view the keyboard back, the menu only once the view shows again", async () => {
  const seen = await page(`
    await F.stage();
    const view = await F.live("a", ${MASTER});
    await pause(200);
    const on = (mark) => F.callsOn(view, mark).filter((command) => command !== "desktop_native_terminal_place");
    const from = F.calls().length;
    F.event(view, { type: "behind", behind: true });
    await F.until(() => F.shown("a").buttons.includes("Jump to latest"));
    F.press("a", "Jump to latest");
    await F.until(() => on(from).includes("desktop_native_terminal_focus"));
    const jumped = on(from);
    const rect = F.screenRect("a");
    const mark = F.calls().length;
    F.event(view, { type: "menu", x: rect.x + 30, y: rect.y + 30 });
    await F.until(() => F.shown("a").menu.length === 2);
    await F.until(() => F.calls("desktop_native_terminal_cover").at(-1)?.args.covered === true);
    document.querySelector("#a [role=menu]").dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
    await F.until(() => on(mark).includes("desktop_native_terminal_focus"));
    await pause(100);
    return { jumped, menu: on(mark), open: F.shown("a").menu.length };
  `);
  assert.deepEqual(seen.jumped, ["desktop_native_terminal_jump", "desktop_native_terminal_focus"]);
  assert.deepEqual(seen.menu, ["desktop_native_terminal_cover", "desktop_native_terminal_cover", "desktop_native_terminal_focus"], "Escape closes the menu, the view shows again, then it gets the keyboard");
  assert.equal(seen.open, 0);
});

test("V243-NATIVE-INPUT E5: a native view that ends with a native code says why in its own words with Open again, and Open again opens a new view", async () => {
  const seen = await page(`
    await F.stage();
    const view = await F.live("a", ${MASTER});
    F.event(view, { type: "ended", code: "native_terminal_gpu", final: true });
    const words = F.words("x", "native_terminal_gpu");
    const generic = F.words("x", "x").ended;
    const ended = await F.until(() => F.shown("a").alerts.some((line) => line.startsWith(words.ended)));
    const pressed = F.press("a", "Open again");
    const reopened = await F.until(() => F.calls("desktop_native_terminal_open").length === 2);
    return { own: words.ended !== generic, ended, pressed, reopened };
  `);
  assert.deepEqual(seen, { own: true, ended: true, pressed: true, reopened: true });
});

test("V243-NATIVE-SEED-REFUSED: a console seed the desktop refuses is marked used, so no later panel pastes it unasked, and its reason stays in an alert until the owner dismisses it (#506 F1)", async () => {
  const seen = await page(`
    await F.stage();
    window.seedUsed = 0;
    const seed = { id: 7, text: "check the build", used: () => { window.seedUsed += 1; } };
    F.fail("paste", { code: "native_terminal_view_unknown" });
    const words = F.words("native_terminal_view_unknown", "x");
    const line = "The drafted message wasn't pasted. " + words.refused;
    F.mount("a", ${MASTER}, { seed });
    await F.until(() => F.calls("desktop_native_terminal_open").length === 1);
    F.event(1, { type: "ready", cols: 80, rows: 24 });
    await F.until(() => F.calls("desktop_native_terminal_paste").length === 1);
    const shown = await F.until(() => F.shown("a").alerts.includes(line + "Dismiss"));
    await pause(2500);
    const kept = F.shown("a").alerts.includes(line + "Dismiss");
    const alert = [...document.querySelectorAll("#a [role=alert]")].find((element) => element.textContent.startsWith(line))?.getBoundingClientRect() ?? null;
    const screen = F.screenRect("a");
    const clear = alert !== null && alert.height > 0 && (alert.bottom <= screen.y || alert.top >= screen.y + screen.height);
    const used = window.seedUsed;
    const dismissed = F.press("a", "Dismiss");
    const gone = await F.until(() => !F.shown("a").alerts.some((text) => text.startsWith(line)));
    return { shown, kept, clear, used, dismissed, gone, pastes: F.calls("desktop_native_terminal_paste").length };
  `);
  assert.deepEqual(seen, { shown: true, kept: true, clear: true, used: 1, dismissed: true, gone: true, pastes: 1 }, "clear: the alert sits outside the screen the native view covers");
});

test("V243-NATIVE-INPUT r2 (#509 F1): a focus waiting under a page dialog is dropped when the view ends, so the view Open again makes gets the keyboard only once it is live", async () => {
  const seen = await page(`
    await F.stage();
    const focus = () => F.calls("desktop_native_terminal_focus").map((entry) => entry.args);
    F.dialog(true);
    F.mount("a", ${MASTER});
    await F.until(() => F.calls("desktop_native_terminal_open").length === 1);
    F.event(1, { type: "ready", cols: 80, rows: 24 });
    await F.until(() => document.querySelector("#a .tg-terminal-note") !== null);
    await pause(200);
    const covered = focus().length;
    F.event(1, { type: "ended", code: "terminal_attach_failed", final: true });
    await F.until(() => F.shown("a").buttons.includes("Open again"));
    F.dialog(false);
    await pause(200);
    F.press("a", "Open again");
    await F.until(() => F.calls("desktop_native_terminal_open").length === 2);
    await pause(300);
    const beforeReady = focus();
    F.event(2, { type: "ready", cols: 80, rows: 24 });
    await F.until(() => focus().length > beforeReady.length);
    await pause(100);
    return { covered, beforeReady, after: focus() };
  `);
  assert.equal(seen.covered, 0, "nothing while the dialog covers the live view");
  assert.deepEqual(seen.beforeReady, [], "the view Open again made is not focused while it connects");
  assert.deepEqual(seen.after, [{ view: 2 }], "it gets the keyboard once, when it goes live");
});

test("V243-NATIVE-INPUT r2 (#509 F2): while a following view connects again after a terminal_closed end, its status line gives the cause and no instruction to open it again", async () => {
  const seen = await page(`
    await F.stage();
    const view = await F.live("a", ${MASTER}, { follow: true });
    F.event(view, { type: "ended", code: "terminal_closed", final: false });
    const ended = F.words("x", "terminal_closed").ended;
    const connecting = await F.until(() => F.shown("a").status.some((line) => line.startsWith("Connecting to the terminal again, try 1 of 30. Last try: ")));
    return { connecting, ended, line: F.shown("a").status.find((line) => line.startsWith("Connecting")) ?? null };
  `);
  assert.equal(seen.connecting, true);
  assert.equal(seen.line, "Connecting to the terminal again, try 1 of 30. Last try: " + seen.ended);
  assert.ok(!/open it again/i.test(seen.line), seen.line);
});

test("V243-NATIVE-INPUT r2 (rv-sec2 #509 N1): with Message box only saved, the app-start preload opens the remembered terminal with keys off, before any panel shows it", async () => {
  const seen = await page(`
    await F.stage({ nodes: [${MASTER}] });
    F.input("box");
    F.preload([{ key: { project: "p1", terminal: "team-1", pane: null }, size: { width: 640, height: 380 } }], ["p1"]);
    if (!(await F.until(() => F.calls("desktop_native_terminal_open").length === 1))) return { loaded: false };
    await pause(200);
    return { loaded: true, keys: F.calls("desktop_native_terminal_open")[0].args.keys, calls: F.calls("desktop_native_terminal_keys").length };
  `);
  assert.deepEqual(seen, { loaded: true, keys: false, calls: 0 });
});

test("V243-NATIVE-INPUT r2 (fix11 05:22): each native open carries the project its terminal is in, which a wall window needs: the panel's project, the remembered key's at app start, or null for a panel without one", async () => {
  const preloaded = await page(`
    await F.stage({ nodes: [${MASTER}] });
    F.preload([{ key: { project: "p1", terminal: "team-1", pane: null }, size: { width: 640, height: 380 } }], ["p1"]);
    if (!(await F.until(() => F.calls("desktop_native_terminal_open").length === 1))) return { loaded: false };
    return { loaded: true, project: F.calls("desktop_native_terminal_open")[0].args.project };
  `);
  assert.deepEqual(preloaded, { loaded: true, project: "p1" });
  const panels = await page(`
    await F.stage();
    F.mount("a", F.agent("sub", "Brook", "team-2"), { projectId: "p2" });
    await F.until(() => F.calls("desktop_native_terminal_open").length === 1);
    F.mount("b", F.agent("sub", "Wren", "team-3"), { projectId: undefined });
    await F.until(() => F.calls("desktop_native_terminal_open").length === 2);
    return F.calls("desktop_native_terminal_open").map((entry) => [entry.args.name, entry.args.project]);
  `);
  assert.deepEqual(panels, [["team-2", "p2"], ["team-3", null]]);
});
