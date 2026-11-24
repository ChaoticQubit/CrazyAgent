import assert from "node:assert/strict";
import { readFileSync, readdirSync } from "node:fs";
import test from "node:test";
import { assertWindowGrants } from "./window-grants.mjs";
import { commandFn, nativeSources, unbound } from "./native-commands.mjs";

const read = (path) => readFileSync(new URL(`../${path}`, import.meta.url), "utf8");

function invokedCommands() {
  const commands = new Set();
  for (const name of readdirSync(new URL("../src/", import.meta.url))) {
    if (!/\.tsx?$/.test(name)) continue;
    for (const match of read(`src/${name}`).matchAll(/(?:invoke|call)<[^(]*>\("(desktop_[a-z_]+)"/g)) {
      commands.add(match[1]);
    }
  }
  return [...commands];
}

test("every command the window invokes is registered in all four native places", () => {
  const build = read("src-tauri/build.rs");
  const lib = read("src-tauri/src/lib.rs");
  const native = nativeSources();
  const handler = lib.slice(lib.indexOf("native_commands! {"));
  const commands = invokedCommands();
  assert.ok(commands.length > 0);
  assertWindowGrants(commands);
  for (const command of commands) {
    assert.ok(build.includes(`"${command}"`), `${command} missing from build.rs`);
    assert.match(native, commandFn(command), `${command} has no tauri command`);
    assert.match(handler, new RegExp(`\\b${command},`), `${command} missing from invoke_handler`);
  }
});

test("a command declared in another src-tauri/src module meets the same rule as one in lib.rs", () => {
  const native = nativeSources();
  assert.ok(native.startsWith(read("src-tauri/src/lib.rs")), "lib.rs is read first");
  assert.match(native, commandFn("desktop_restore_recovery"), "restore.rs is read");
  assert.doesNotMatch(read("src-tauri/src/lib.rs"), commandFn("desktop_restore_recovery"), "the restore commands live in restore.rs");
  const lib = read("src-tauri/src/lib.rs");
  const declared = (text) => `${lib}\n${text}`;
  assert.deepEqual(unbound(["desktop_trial"], declared("async fn desktop_trial(webview: Webview) {}")), ["desktop_trial"], "no attribute");
  assert.deepEqual(unbound(["desktop_trial"], declared("#[tauri::command]\n// moved\nfn desktop_trial() {}")), ["desktop_trial"], "attribute not on the fn");
  assert.deepEqual(unbound(["desktop_trial"], declared("#[tauri::command]\npub async fn desktop_trial() {}")), ["desktop_trial"], "pub is not the rule");
  assert.deepEqual(unbound(["desktop_trial"], declared("#[tauri::command]\npub(crate) async fn desktop_trial() {}")), []);
  assert.deepEqual(unbound(["desktop_trial"], declared("#[tauri::command]\nasync fn desktop_trial() {}")), []);
});

test("the window lists harnesses through the engine catalog command", () => {
  const client = read("src/engine-client.ts");
  assert.match(client, /export function harnessCatalog\(\): Promise<HarnessEntry\[\]>/);
  assert.ok(invokedCommands().includes("desktop_harness_catalog"));
});
