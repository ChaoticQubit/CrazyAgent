import assert from "node:assert/strict";
import test from "node:test";
import { bundle, withPage } from "./browser.mjs";

const fixture = await bundle(new URL("./fixtures/app-error-boundary.mjs", import.meta.url).pathname);
const WAIT_MS = 5000;
const HEADING = "This window stopped working";
const RUNNING = { desktop_connect: { connected: true, stuck_sign_in: null }, desktop_owner_profile: { name: "Mira Okafor" } };
const HEALTHY = { ...RUNNING, desktop_project_list: { items: [], more: false } };
const BAD_PROJECTS = { ...RUNNING, desktop_project_list: { items: 5, more: false } };
const BAD_GRAPH = { ...RUNNING, desktop_team_graph: { nodes: 5 }, desktop_terminals_per_window: 4 };
const WALL = "#/terminals/alpha/1/Alpha";
const pause = (ms) => new Promise((done) => setTimeout(done, ms));

/** Boots the real src/main.tsx in the page. Inputs: the page, the engine's answers by command
 * and the window's hash. Any other command is not in this build. Output: nothing. */
async function boot(page, answers, hash = "") {
  await page.evaluate(`(() => {
    const answers = ${JSON.stringify(answers)};
    window.__TAURI_INTERNALS__ = {
      plugins: {},
      transformCallback: () => 0,
      async invoke(command) {
        if (command in answers) return answers[command];
        throw "Command " + command + " not found";
      },
    };
    window.__notReloaded = true;
    location.hash = ${JSON.stringify(hash)};
    document.body.innerHTML = '<div id="root"></div>';
  })()`);
  await page.evaluate(fixture);
}

/** Waits for an expression to hold. Inputs: the page, the expression and what it stands for.
 * Output: nothing; fails the row when the wait runs out. */
async function until(page, expression, what) {
  const deadline = Date.now() + WAIT_MS;
  while (Date.now() < deadline) {
    if (await page.evaluate(expression)) return;
    await pause(25);
  }
  assert.fail(`timed out waiting for ${what}; the window holds ${await page.evaluate("document.getElementById('root')?.childElementCount")} elements`);
}

const shown = (page) => until(page, `document.querySelector("h1")?.textContent === ${JSON.stringify(HEADING)}`, "the failure message");

/** Waits for the page's script state to be gone, which only a real reload does. Input: the
 * page. Output: nothing; fails the row when the page stays as it was. */
async function reloaded(page) {
  const deadline = Date.now() + WAIT_MS;
  while (Date.now() < deadline) {
    if (await page.evaluate("window.__notReloaded !== true").catch(() => false)) return;
    await pause(25);
  }
  assert.fail("the window did not reload");
}

async function press(page, key, code, keyCode, text) {
  await page.send("Input.dispatchKeyEvent", { type: "keyDown", key, code, windowsVirtualKeyCode: keyCode, ...(text ? { text } : {}) });
  await page.send("Input.dispatchKeyEvent", { type: "keyUp", key, code, windowsVirtualKeyCode: keyCode });
}

async function clickReload(page) {
  const box = await page.evaluate(`(() => {
    const found = [...document.querySelectorAll("button")].find((item) => item.textContent === "Reload");
    if (!found) return null;
    const rect = found.getBoundingClientRect();
    return { x: rect.x + rect.width / 2, y: rect.y + rect.height / 2 };
  })()`);
  assert.ok(box, "a Reload button is on screen");
  await page.send("Input.dispatchMouseEvent", { type: "mouseMoved", x: box.x, y: box.y });
  await page.send("Input.dispatchMouseEvent", { type: "mousePressed", x: box.x, y: box.y, button: "left", clickCount: 1 });
  await page.send("Input.dispatchMouseEvent", { type: "mouseReleased", x: box.x, y: box.y, button: "left", clickCount: 1 });
}

test("a healthy start draws the app and no failure message", async () => {
  await withPage(async (page) => {
    await boot(page, HEALTHY);
    await until(page, `document.querySelector(".sidebar") !== null`, "the sidebar");
    assert.notEqual(await page.evaluate(`document.querySelector("h1")?.textContent ?? ""`), HEADING);
  });
});

test("a throw while drawing the app leaves a plain message, not a blank window", async () => {
  await withPage(async (page) => {
    await boot(page, BAD_PROJECTS);
    await shown(page);
    const words = await page.evaluate(`document.querySelector('[role="alert"]')?.textContent ?? ""`);
    assert.match(words, /Your projects and agents aren't affected\./u);
    assert.match(words, /If this comes back, restart the app\./u);
    assert.match(words, /Anything you typed and didn't save will be lost\./u);
    await pause(300);
    assert.equal(await page.evaluate("window.__notReloaded"), true, "the window does not reload by itself");
  });
});

test("the failure message shows no raw error text", async () => {
  await withPage(async (page) => {
    await boot(page, BAD_PROJECTS);
    await shown(page);
    const words = await page.evaluate("document.body.innerText");
    assert.doesNotMatch(words, /TypeError|is not a function/u);
  });
});

test("a throw while drawing a terminal window leaves the same message", async () => {
  await withPage(async (page) => {
    await boot(page, BAD_GRAPH, WALL);
    await shown(page);
    assert.equal(await page.evaluate(`document.querySelector(".tw") === null`), true);
  });
});

test("Reload reloads the window when pressed with the mouse", async () => {
  await withPage(async (page) => {
    await boot(page, BAD_PROJECTS);
    await shown(page);
    await clickReload(page);
    await reloaded(page);
  });
});

test("Reload has the focus as soon as the message shows and Enter reloads the window", async () => {
  await withPage(async (page) => {
    await boot(page, BAD_PROJECTS);
    await shown(page);
    assert.equal(await page.evaluate("document.activeElement?.textContent"), "Reload");
    await press(page, "Enter", "Enter", 13, "\r");
    await reloaded(page);
  });
});
