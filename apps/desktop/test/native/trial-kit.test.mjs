import assert from "node:assert/strict";
import { mkdtempSync, readFileSync, realpathSync, rmSync, statSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import * as kit from "./trial-kit.mjs";
import { terminalKeys } from "./trial-kit.mjs";

const PICKS = "CRAZYAGENT_TRIAL_PICKS";
const PURPOSES = ["skill", "project", "projects_home", "memory", "plugin"];
const SEAM = new URL("../../src-tauri/src/bridge/trial_pick.rs", import.meta.url);

/** A page scope shaped as Tauri 2.12.4 shapes it: __TAURI_INTERNALS__ is defined on the window
 * with only a value (manager/webview.rs) and so is its invoke (scripts/core.js), so neither can
 * be replaced. Output: the scope and its invoke. */
function tauriPage() {
  const scope = {};
  const invoke = async (cmd) => `native ${cmd}`;
  Object.defineProperty(scope, "__TAURI_INTERNALS__", { value: { plugins: {} } });
  Object.defineProperty(scope.__TAURI_INTERNALS__, "invoke", { value: invoke });
  return { scope, invoke };
}

/** Runs a body with the picks folder named in the environment and a WebDriver whose execute
 * runs the page function on a Tauri-shaped page. Input: the body, given the picks folder and
 * the page. Output: the body's result; the folder and the variable are removed after. */
async function withPicks(body) {
  const root = realpathSync(mkdtempSync(join(tmpdir(), "ca-kit-picks-")));
  const picks = join(root, "picks");
  const page = tauriPage();
  const executed = [];
  globalThis.browser = { execute: async (fn, ...args) => { executed.push(fn.name); return fn(...args, page.scope); } };
  process.env[PICKS] = picks;
  try {
    return await body(picks, page, executed);
  } finally {
    delete process.env[PICKS];
    delete globalThis.browser;
    rmSync(root, { recursive: true, force: true });
  }
}

test("chooseFolder arms a pick on a page whose Tauri invoke is locked as Tauri 2.12.4 locks it, without touching that invoke", async () => {
  await withPicks(async (picks, page, executed) => {
    await kit.chooseFolder("/Users/owner/Projects/ca-repo", "project");
    const answer = join(picks, "project");
    assert.equal(readFileSync(answer, "utf8"), "/Users/owner/Projects/ca-repo\n");
    assert.equal(statSync(answer).mode & 0o777, 0o600);
    assert.equal(statSync(picks).mode & 0o777, 0o700);
    assert.equal(page.scope.__TAURI_INTERNALS__.invoke, page.invoke);
    assert.deepEqual(executed, [], "the page is never asked to wrap its invoke");
  });
});

test("a second answer of a purpose the app has not taken yet is refused, and another purpose still arms", async () => {
  await withPicks(async (picks) => {
    await kit.chooseFolder("/a", "project");
    await assert.rejects(kit.chooseFolder("/b", "project"), /an earlier project pick still waits/);
    assert.equal(readFileSync(join(picks, "project"), "utf8"), "/a\n");
    await kit.chooseFolder("/c", "skill");
    assert.equal(readFileSync(join(picks, "skill"), "utf8"), "/c\n");
  });
});

test("chooseFolder refuses a relative path, a control character or an unknown purpose before writing", async () => {
  await withPicks(async (picks) => {
    await assert.rejects(kit.chooseFolder("CrazyAgent/ca-trial-4"), /an absolute path/);
    await assert.rejects(kit.chooseFolder("/a\nb"), /without control characters/);
    await assert.rejects(kit.chooseFolder("/a", "repo"), /purpose is one of skill, project, projects_home, memory, plugin/);
    assert.throws(() => statSync(picks), { code: "ENOENT" });
  });
});

test("chooseFolder without the picks folder says the app cannot take a pick", async () => {
  await assert.rejects(kit.chooseFolder("/a", "project"), new RegExp(`needs ${PICKS}`));
});

test("the kit and the app name the picks folder and the answer files alike", () => {
  assert.equal(kit.PICKS_VARIABLE, PICKS);
  const seam = readFileSync(SEAM, "utf8");
  assert.ok(seam.includes(`pub const PICKS_VARIABLE: &str = "${PICKS}";`), "trial_pick.rs reads the same variable");
  for (const purpose of PURPOSES) assert.match(seam, new RegExp(`=> "${purpose}",`), purpose);
});

const SHIFT = "\uE008";
const ENTER = "\uE007";

test("terminal keys type N-D10's lines once each, Shift+Enter as a Shift key press and Enter as a carriage return", () => {
  const lines = ["Trial 4 check N-D10, first line.", "Second line: reply with the single word ok."];
  assert.deepEqual(terminalKeys(lines[0]), [{ text: lines[0] }]);
  assert.deepEqual(terminalKeys([SHIFT, ENTER]), [{ key: "Enter", shiftKey: true }]);
  assert.deepEqual(terminalKeys(ENTER), [{ text: "\r", alone: true }]);
  assert.deepEqual(terminalKeys([lines[0], SHIFT, ENTER, SHIFT, lines[1], ENTER, "x"]), [{ text: lines[0] }, { key: "Enter", shiftKey: true }, { text: lines[1] }, { text: "\r", alone: true }, { text: "x" }]);
});

test("terminal keys hold WebDriver modifiers until pressed again or released, within one call", () => {
  assert.deepEqual(terminalKeys(["\uE009", "c", "\uE000", "c"]), [{ text: "\u0003c" }]);
  assert.deepEqual(terminalKeys([SHIFT, "ab", SHIFT, "c"]), [{ text: "ABc" }]);
  assert.deepEqual(terminalKeys([SHIFT]), []);
  assert.deepEqual(terminalKeys("a"), [{ text: "a" }]);
  assert.deepEqual(terminalKeys(["\uE004", "\uE003", "\uE00C", "\uE006"]), [{ text: "\t\u007f\u001b" }, { text: "\r", alone: true }]);
});

test("terminal keys refuse what the terminal path cannot send instead of typing something else", () => {
  assert.throws(() => terminalKeys(["\uE00A", "x"]), /no Alt or Meta chord/);
  assert.throws(() => terminalKeys(["\uE03D", "c"]), /no Alt or Meta chord/);
  assert.throws(() => terminalKeys("\uE015"), /does not send the WebDriver key U\+E015/);
  assert.throws(() => terminalKeys([SHIFT, "1"]), /Shift only with a letter or Enter/);
  assert.throws(() => terminalKeys(["\uE009", ENTER]), /Enter with Control/);
  assert.throws(() => terminalKeys(["\uE009", "1"]), /Control only with a letter/);
  assert.throws(() => terminalKeys([7]), /strings/);
});

test("browser.keys with Meta (Cmd) is refused on macOS before any key goes out, other keys and systems pass, and the kit's keys refuses first (TRIAL-KIT-WEBDRIVER-META)", () => {
  assert.equal(typeof kit.refusedKeys, "function", "the kit names why a key sequence never goes out");
  assert.match(kit.refusedKeys(["\uE03D", "c"], "darwin"), /ABOUT-THIS-MAC/);
  assert.notEqual(kit.refusedKeys("a\uE053", "darwin"), null, "the right Meta key too");
  assert.notEqual(kit.refusedKeys(["x", "\uE03D"], "darwin"), null, "a Meta in any part");
  for (const value of ["a", ["\uE009", "c"], [SHIFT, ENTER], "\uE00A", []]) assert.equal(kit.refusedKeys(value, "darwin"), null, JSON.stringify(value));
  assert.equal(kit.refusedKeys(["\uE03D", "c"], "linux"), null, "off a Mac a Meta key reaches no Mac menu bar");
  const source = readFileSync(new URL("./trial-kit.mjs", import.meta.url), "utf8");
  assert.match(source, /overwriteCommand\("keys", async function keys\(webdriverKeys, value\) \{\s*const refused = refusedKeys\(value\);\s*if \(refused !== null\) throw /u, "the kit's keys refuses before it types or calls WebDriver");
});

test("browser.keys refuses on macOS the WebdriverIO key names that become Cmd there, a whole part Meta or Command and Key.Ctrl, and passes words that only start like them (TRIAL-KIT-WEBDRIVER-META, #153 F1)", () => {
  for (const name of ["Meta", "Command", "WDIO_CONTROL"]) assert.match(kit.refusedKeys([name, "v"], "darwin") ?? "", /ABOUT-THIS-MAC/, `${name} is Cmd on macOS`);
  for (const value of [["Metadata"], ["a Command"], ["Control", "c"], ["WDIO_CONTROLS"]]) assert.equal(kit.refusedKeys(value, "darwin"), null, JSON.stringify(value));
  assert.equal(kit.refusedKeys(["WDIO_CONTROL", "v"], "linux"), null, "off a Mac Key.Ctrl is Control");
});
