import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import "./load-source.mjs";

const settings = await import("../src/screens/project/settings.tsx");
const api = await import("../src/api.ts");

test("the sandbox section opens from its route and adds only absolute, new folders up to the limit", () => {
  assert.equal(settings.settingsSection("sandbox"), "sandbox");
  assert.deepEqual(settings.addReadPath([], "  /Users/me/shared  "), { paths: ["/Users/me/shared"] });
  assert.deepEqual(settings.addReadPath([], "~/shared"), { error: "Type the full path of a folder, starting with /." });
  assert.deepEqual(settings.addReadPath([], "relative/folder"), { error: "Type the full path of a folder, starting with /." });
  assert.deepEqual(settings.addReadPath(["/a"], "/a"), { error: "That folder is already in the list." });
  assert.deepEqual(settings.addReadPath([], "/bad\npath"), { error: "Each folder must be one plain path of up to 1024 characters." });
  const full = Array.from({ length: settings.READ_PATHS_MAX }, (_, index) => `/f${index}`);
  assert.deepEqual(settings.addReadPath(full, "/one-more"), { error: "A project can share at most 8 extra folders." });
});

test("every engine refusal for a read-only folder has its own sentence", () => {
  for (const code of ["read_path_not_absolute", "read_path_missing", "read_path_not_folder", "read_path_holds_home", "read_path_credentials",
    "read_path_engine_data", "read_path_refused", "read_path_owner_unknown", "read_paths_too_many", "read_paths_invalid"]) {
    assert.ok(api.KNOWN_ERROR_CODES.includes(code), code);
  }
  const engine = readFileSync(new URL("../../../crates/crazyagent/src/sandbox/extra_paths.rs", import.meta.url), "utf8");
  for (const code of engine.matchAll(/"(read_paths?_[a-z_]+)"/g)) assert.ok(api.KNOWN_ERROR_CODES.includes(code[1]), code[1]);
});

test("a save refused because the folders changed reads them again instead of resending the old version", () => {
  const receipt = (outcome, rejection = null) => ({ kind: "ok", value: { pending: false, outcome, rejection } });
  assert.equal(settings.savedStale(receipt("rejected", "stale_revision")), true, "another writer changed the folders since this screen read them");
  assert.equal(settings.savedStale({ kind: "error", code: "stale_revision", message: "" }), true);
  assert.equal(settings.savedStale(receipt("applied")), false);
  assert.equal(settings.savedStale(receipt("rejected", "read_path_credentials")), false, "an unsafe folder keeps the owner's list to fix");
  assert.equal(settings.savedStale({ kind: "not_ready" }), false);
  assert.equal(settings.savedStale({ kind: "ok", value: { pending: true, outcome: "uncertain", rejection: null } }), false, "an unconfirmed save is not a stale one");
  assert.match(settings.READ_PATHS_STALE, /changed since you opened them\. The list now shows the saved folders/);
  const source = readFileSync(new URL("../src/screens/project/settings.tsx", import.meta.url), "utf8");
  const save = source.slice(source.indexOf("  async function save() {\n    if (saved === null) return;"));
  assert.match(save, /if \(savedStale\(result\)\) \{\n      const shown = await reread\(\);/, "the stale save reads the folders and their version again");
  assert.ok(save.indexOf("savedStale(result)") < save.indexOf("failureCopy(result)"), "before the plain refusal copy");
  const reread = source.slice(source.indexOf("  async function reread()"), source.indexOf("  async function save() {\n    if (saved === null)"));
  assert.match(reread, /setSaved\(fresh\.value\);\n    setPaths\(fresh\.value\.paths\);/);
});

test("a hidden folders save sends the version it read, and a stale one reads the folders again instead of un-hiding one", () => {
  assert.match(settings.HIDDEN_PATHS_STALE, /changed since you opened them\. The list now shows the saved folders/);
  const source = readFileSync(new URL("../src/screens/project/settings.tsx", import.meta.url), "utf8");
  const start = source.indexOf("function HiddenSection(");
  const hidden = source.slice(start, source.indexOf("\n}\n", start));
  assert.match(hidden, /useState<HiddenPaths \| null>\(null\)/);
  assert.match(hidden, /saveHiddenPaths\(data\.id, paths, saved\.version, nextGeneration\(\)\)/, "the save sends the version the screen read");
  assert.match(hidden, /if \(savedStale\(result\)\) \{\n      const shown = await reread\(\);/, "a stale save reads the folders and their version again");
  const save = hidden.slice(hidden.indexOf("  async function save() {"));
  assert.ok(save.indexOf("savedStale(result)") < save.indexOf("failureCopy(result)"), "before the plain refusal copy");
  assert.match(hidden, /setSaved\(fresh\.value\);\n    setPaths\(fresh\.value\.paths\);/);
  assert.ok(!hidden.includes("setSaved(paths)"), "an applied save reads the folders and their new version again");
  const api = readFileSync(new URL("../src/api-projects.ts", import.meta.url), "utf8");
  assert.match(api, /export function saveHiddenPaths\(id: string, paths: string\[\], version: number, generation: number\)/);
  assert.match(api, /call<IntentReceipt>\("desktop_set_project_hidden_paths", \{ id, version, paths, generation \}\)/);
});
