import assert from "node:assert/strict";
import { test } from "node:test";
import { armPick, chooseFolder, terminalKeys } from "./trial-kit.mjs";

/** A page scope whose Tauri invoke records every call it is given and answers with the
 * command name, as the app's own invoke would reach the native side. Output: the scope and
 * its call list. */
function page() {
  const calls = [];
  const scope = { __TAURI_INTERNALS__: { invoke: async (cmd, args) => { calls.push([cmd, args]); return `native ${cmd}`; } } };
  return { scope, calls };
}

test("an armed pick answers the next folder chooser of its purpose once, and every other call reaches the app's invoke", async () => {
  const { scope, calls } = page();
  assert.equal(armPick("/Users/owner/CrazyAgent/ca-trial-4", "project", scope), true);
  const invoke = scope.__TAURI_INTERNALS__.invoke;
  assert.equal(await invoke("desktop_pick_folder", { purpose: "skill" }), "native desktop_pick_folder");
  assert.equal(await invoke("desktop_team_graph", { id: "p" }), "native desktop_team_graph");
  assert.equal(await invoke("desktop_pick_folder", { purpose: "project" }), "/Users/owner/CrazyAgent/ca-trial-4");
  assert.equal(await invoke("desktop_pick_folder", { purpose: "project" }), "native desktop_pick_folder");
  assert.deepEqual(calls, [["desktop_pick_folder", { purpose: "skill" }], ["desktop_team_graph", { id: "p" }], ["desktop_pick_folder", { purpose: "project" }]]);
});

test("arming again replaces the waiting pick and wraps the invoke only once", async () => {
  const { scope, calls } = page();
  armPick("/a", "project", scope);
  const wrapped = scope.__TAURI_INTERNALS__.invoke;
  armPick("/b", "project", scope);
  assert.equal(scope.__TAURI_INTERNALS__.invoke, wrapped);
  assert.equal(await wrapped("desktop_pick_folder", { purpose: "project" }), "/b");
  assert.deepEqual(calls, []);
});

test("a page whose invoke cannot be replaced reports it instead of leaving the chooser open", () => {
  const scope = { __TAURI_INTERNALS__: Object.freeze({ invoke: async () => "native" }) };
  assert.equal(armPick("/a", "project", scope), false);
});

test("chooseFolder refuses a relative path or an unknown purpose before touching the window", async () => {
  await assert.rejects(chooseFolder("CrazyAgent/ca-trial-4"), /an absolute path/);
  await assert.rejects(chooseFolder("/a", "repo"), /purpose is one of skill, project, projects_home, memory, plugin/);
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
