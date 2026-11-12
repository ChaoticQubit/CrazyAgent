import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

const read = (path) => readFileSync(new URL(`../${path}`, import.meta.url), "utf8");
const permissions = (name) => JSON.parse(read(`src-tauri/capabilities/${name}.json`)).permissions;
const grant = (command) => `allow-${command.replaceAll("_", "-")}`;

/**
 * Returns the names in WALL_ONLY of tests/command_permissions.rs, so the list has one home.
 * Output: the command names. Stops with an assertion if the constant cannot be read, or its
 * declared length and its names disagree.
 */
export function wallOnlyCommands() {
  const found = read("src-tauri/tests/command_permissions.rs").match(/const WALL_ONLY: \[&str; (\d+)\] = \[([^\]]*)\];/);
  assert.ok(found, "WALL_ONLY not found in command_permissions.rs");
  const names = [...found[2].matchAll(/"(desktop_[a-z_]+)"/g)].map((match) => match[1]);
  assert.ok(names.length > 0, "WALL_ONLY has no names");
  assert.equal(names.length, Number(found[1]), "WALL_ONLY length and names disagree");
  return names;
}

/**
 * Asserts each command has the grant of the window that may call it. A wall-only command must be
 * in the wall capability and absent from main-window.json, any other must be in main-window.json.
 * Input: the commands the window files call. Output: none, stops with an assertion on a wrong grant.
 */
export function assertWindowGrants(commands) {
  const main = permissions("main-window");
  const wall = permissions("terminal-wall");
  const wallOnly = wallOnlyCommands();
  for (const command of wallOnly) {
    assert.ok(wall.includes(grant(command)), `${command} missing from terminal-wall.json`);
    assert.ok(!main.includes(grant(command)), `${command} is wall-only but in main-window.json`);
  }
  for (const command of commands) {
    if (!wallOnly.includes(command)) assert.ok(main.includes(grant(command)), `${command} missing from main-window.json`);
  }
}
