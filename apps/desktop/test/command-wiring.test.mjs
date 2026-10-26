import assert from "node:assert/strict";
import { readFileSync, readdirSync } from "node:fs";
import test from "node:test";

const read = (path) => readFileSync(new URL(`../${path}`, import.meta.url), "utf8");

function sources(dir = "src") {
  const files = [];
  for (const entry of readdirSync(new URL(`../${dir}/`, import.meta.url), { withFileTypes: true })) {
    const path = `${dir}/${entry.name}`;
    if (entry.isDirectory()) files.push(...sources(path));
    else if (/\.tsx?$/.test(entry.name)) files.push(path);
  }
  return files;
}

function called() {
  const commands = new Set();
  for (const file of sources()) {
    for (const match of read(file).matchAll(/\(\s*"(desktop_[a-z_]+)"/g)) commands.add(match[1]);
  }
  return commands;
}

function registered() {
  const lib = read("src-tauri/src/lib.rs");
  const handler = lib.slice(lib.indexOf("generate_handler!["));
  const list = [...handler.slice(0, handler.indexOf("])")).matchAll(/\b(desktop_[a-z_]+)\b/g)].map((match) => match[1]);
  return { lib, list };
}

test("every command any window file calls, through invoke or call, is registered in all four native places", () => {
  const build = read("src-tauri/build.rs");
  const allowed = JSON.parse(read("src-tauri/capabilities/main-window.json")).permissions;
  const { lib, list } = registered();
  const commands = called();
  assert.ok(commands.size > 100);
  for (const command of commands) {
    assert.ok(list.includes(command), `${command} missing from invoke_handler`);
    assert.ok(build.includes(`"${command}"`), `${command} missing from build.rs`);
    assert.ok(allowed.includes(`allow-${command.replaceAll("_", "-")}`), `${command} missing from main-window.json`);
    assert.match(lib, new RegExp(`#\\[tauri::command\\]\\s*(?:async )?fn ${command}\\(`), `${command} has no tauri command`);
  }
});

test("every registered command is called by the window and registered once", () => {
  const { list } = registered();
  const commands = called();
  assert.equal(new Set(list).size, list.length, "a command is listed twice in invoke_handler");
  for (const command of list) assert.ok(commands.has(command), `${command} is registered but never called`);
  const build = [...read("src-tauri/build.rs").matchAll(/"(desktop_[a-z_]+)"/g)].map((match) => match[1]);
  assert.equal(new Set(build).size, build.length, "a command is listed twice in build.rs");
  const allowed = JSON.parse(read("src-tauri/capabilities/main-window.json")).permissions.filter((item) => typeof item === "string");
  assert.equal(new Set(allowed).size, allowed.length, "a permission is listed twice in main-window.json");
});

test("the intake, GitHub and setup commands send the argument names their Rust commands take", () => {
  const client = read("src/engine-client.ts");
  for (const [command, args] of [
    ["desktop_set_projects_home", "{ path, generation }"], ["desktop_move_projects_home", "{ path, generation }"],
    ["desktop_folder_size", "{ path }"], ["desktop_add_existing_folder", "{ source, mode, generation }"],
    ["desktop_start_onboarding", "{ path, pool, generation }"], ["desktop_onboarding", "{ id }"],
    ["desktop_github_token", "{ token }"], ["desktop_github_repos", "{ page }"], ["desktop_clone_repo", "{ fullName }"],
    ["desktop_run_setup", "{ pack, generation }"], ["desktop_set_pack_entry", "{ id, on, generation }"], ["desktop_remove_pack_entry", "{ id, generation }"],
    ["desktop_team_graph", "{ id }"],
  ]) assert.ok(client.includes(`("${command}", ${args})`), command);
  const lib = read("src-tauri/src/lib.rs");
  for (const [command, params] of [
    ["desktop_move_projects_home", ["path: String", "generation: u64"]],
    ["desktop_add_existing_folder", ["source: String", "mode: AddMode", "generation: u64"]],
    ["desktop_start_onboarding", ["path: String", "pool: String", "generation: u64"]],
    ["desktop_clone_repo", ["full_name: String"]],
    ["desktop_set_pack_entry", ["id: String", "on: bool", "generation: u64"]],
    ["desktop_run_setup", ["pack: Vec<String>", "generation: u64"]],
    ["desktop_remove_pack_entry", ["id: String", "generation: u64"]],
  ]) {
    const start = lib.indexOf(`fn ${command}(`);
    assert.ok(start >= 0, command);
    const signature = lib.slice(start, lib.indexOf(")", start));
    for (const param of params) assert.ok(signature.includes(param), `${command} takes ${param}`);
  }
});

test("onboarding steps and setup states use the engine's tags", () => {
  const client = read("src/engine-client.ts");
  assert.match(client, /export type OnboardingStep = \{ kind: "waiting" \} \| \{ kind: "running" \} \| \{ kind: "done" \} \| \{ kind: "off" \} \| \{ kind: "failed"; reason: string \};/);
  assert.match(client, /DependencyState =[\s\S]*?\{ state: "found"; version: string \}/);
  assert.match(client, /PackState =[\s\S]*?\{ state: "available" \}/);
  const engine = readFileSync(new URL("../../../crates/crazyagent/src/onboarding.rs", import.meta.url), "utf8");
  assert.match(engine, /#\[serde\(tag = "kind", rename_all = "snake_case", deny_unknown_fields\)\]\s*pub enum Step/);
});
