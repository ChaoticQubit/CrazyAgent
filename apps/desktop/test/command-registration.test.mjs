import assert from "node:assert/strict";
import { readFileSync, readdirSync } from "node:fs";
import test from "node:test";

const read = (path) => readFileSync(new URL(`../${path}`, import.meta.url), "utf8");

function invokedCommands() {
  const commands = new Set();
  for (const name of readdirSync(new URL("../src/", import.meta.url))) {
    if (!/\.tsx?$/.test(name)) continue;
    for (const match of read(`src/${name}`).matchAll(/invoke<[^(]*>\("(desktop_[a-z_]+)"/g)) {
      commands.add(match[1]);
    }
  }
  return [...commands];
}

test("every command the window invokes is registered in all four native places", () => {
  const build = read("src-tauri/build.rs");
  const lib = read("src-tauri/src/lib.rs");
  const allowed = JSON.parse(read("src-tauri/capabilities/main-window.json")).permissions;
  const handler = lib.slice(lib.indexOf("generate_handler!["));
  const commands = invokedCommands();
  assert.ok(commands.length > 0);
  for (const command of commands) {
    assert.ok(build.includes(`"${command}"`), `${command} missing from build.rs`);
    assert.ok(allowed.includes(`allow-${command.replaceAll("_", "-")}`), `${command} missing from main-window.json`);
    assert.match(lib, new RegExp(`#\\[tauri::command\\]\\s*(?:async )?fn ${command}\\(`), `${command} has no tauri command`);
    assert.match(handler, new RegExp(`\\b${command},`), `${command} missing from invoke_handler`);
  }
});

test("the window lists harnesses through the engine catalog command", () => {
  const client = read("src/engine-client.ts");
  assert.match(client, /export function harnessCatalog\(\): Promise<HarnessEntry\[\]>/);
  assert.ok(invokedCommands().includes("desktop_harness_catalog"));
});
