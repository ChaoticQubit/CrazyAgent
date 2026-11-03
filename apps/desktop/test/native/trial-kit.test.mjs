import assert from "node:assert/strict";
import { test } from "node:test";
import { armPick, chooseFolder } from "./trial-kit.mjs";

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
